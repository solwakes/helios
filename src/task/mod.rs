/// Cooperative & preemptive multitasking for Helios.
/// Each task is a graph node — the graph model IS the process model.
/// Tasks can yield cooperatively via `yield_now()`, and will also be
/// preempted by the timer interrupt handler if they don't yield.

use alloc::string::String;
use alloc::vec::Vec;
use core::arch::global_asm;

use crate::arch::riscv64 as arch;
use crate::graph;
use crate::graph::NodeType;

/// Stack size per task: 16 KiB.
const TASK_STACK_SIZE: usize = 16 * 1024;

/// Maximum number of tasks.
const MAX_TASKS: usize = 32;

// ---------------------------------------------------------------------------
// Task structures
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Ready,
    Running,
    Done,
}

impl core::fmt::Display for TaskState {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            TaskState::Ready => write!(f, "ready"),
            TaskState::Running => write!(f, "running"),
            TaskState::Done => write!(f, "done"),
        }
    }
}

/// Callee-saved register context for cooperative switching.
/// Only ra, sp, s0-s11 need to be saved — caller-saved regs are
/// handled by the normal function call ABI.
#[repr(C)]
pub struct TaskContext {
    pub ra: usize,
    pub sp: usize,
    pub s: [usize; 12], // s0-s11
}

impl TaskContext {
    pub const fn zero() -> Self {
        TaskContext {
            ra: 0,
            sp: 0,
            s: [0; 12],
        }
    }
}

pub struct Task {
    pub id: usize,
    pub name: String,
    pub state: TaskState,
    pub context: TaskContext,
    pub stack: Vec<u8>,
    pub graph_node_id: u64,
    /// Number of times this task has been preempted by the timer.
    pub preempt_count: usize,
}

// ---------------------------------------------------------------------------
// Context switch — pure assembly
// ---------------------------------------------------------------------------

global_asm!(
    r#"
.align 4
.globl switch_context
switch_context:
    # a0 = old: *mut TaskContext
    # a1 = new: *const TaskContext
    # Save callee-saved registers to old context
    sd ra,  0*8(a0)
    sd sp,  1*8(a0)
    sd s0,  2*8(a0)
    sd s1,  3*8(a0)
    sd s2,  4*8(a0)
    sd s3,  5*8(a0)
    sd s4,  6*8(a0)
    sd s5,  7*8(a0)
    sd s6,  8*8(a0)
    sd s7,  9*8(a0)
    sd s8,  10*8(a0)
    sd s9,  11*8(a0)
    sd s10, 12*8(a0)
    sd s11, 13*8(a0)

    # Restore callee-saved registers from new context
    ld ra,  0*8(a1)
    ld sp,  1*8(a1)
    ld s0,  2*8(a1)
    ld s1,  3*8(a1)
    ld s2,  4*8(a1)
    ld s3,  5*8(a1)
    ld s4,  6*8(a1)
    ld s5,  7*8(a1)
    ld s6,  8*8(a1)
    ld s7,  9*8(a1)
    ld s8,  10*8(a1)
    ld s9,  11*8(a1)
    ld s10, 12*8(a1)
    ld s11, 13*8(a1)

    ret
"#
);

extern "C" {
    fn switch_context(old: *mut TaskContext, new: *const TaskContext);
}

// ---------------------------------------------------------------------------
// Global task list (single-hart, no lock needed)
// ---------------------------------------------------------------------------

static mut TASKS: Option<Vec<Task>> = None;
static mut CURRENT_TASK: usize = 0;
static mut NEXT_TASK_ID: usize = 0;

/// Well-known graph node ID for the "tasks" directory.
/// Set during bootstrap.
static mut TASKS_NODE_ID: u64 = 0;

#[allow(static_mut_refs)]
fn tasks() -> &'static Vec<Task> {
    unsafe { TASKS.as_ref().expect("task subsystem not initialized") }
}

#[allow(static_mut_refs)]
fn tasks_mut() -> &'static mut Vec<Task> {
    unsafe { TASKS.as_mut().expect("task subsystem not initialized") }
}

// ---------------------------------------------------------------------------
// Task entry trampoline
// ---------------------------------------------------------------------------

/// Entry point for newly spawned tasks. When switch_context jumps here
/// (via ra), s0 contains the function pointer to call.
#[no_mangle]
extern "C" fn task_entry() {
    let fp: usize;
    unsafe { core::arch::asm!("mv {}, s0", out(reg) fp) };
    let f: fn() = unsafe { core::mem::transmute(fp) };
    f();
    current_task_done();
    yield_now();
    // Should never reach here, but just in case:
    loop {
        unsafe { core::arch::asm!("wfi") };
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Initialize the task subsystem. Creates task #0 for the shell/idle loop.
/// Must be called after graph::init().
#[allow(static_mut_refs)]
pub fn init() {
    // Create the "tasks" directory node in the graph
    let g = graph::get_mut();
    let tasks_id = g.create_node(NodeType::Directory, "tasks");
    // Link from root (node 1) to tasks
    g.add_edge(1, "child", tasks_id);
    unsafe { TASKS_NODE_ID = tasks_id; }

    // Create task 0 (shell) — its context is a dummy; it's already running
    let shell_node_id = g.create_node(NodeType::System, "shell");
    g.add_edge(tasks_id, "child", shell_node_id);

    let shell_task = Task {
        id: 0,
        name: String::from("shell"),
        state: TaskState::Running,
        context: TaskContext::zero(),
        stack: Vec::new(), // shell uses the kernel boot stack
        graph_node_id: shell_node_id,
        preempt_count: 0,
    };

    unsafe {
        TASKS = Some(Vec::new());
        NEXT_TASK_ID = 1;
    }
    tasks_mut().push(shell_task);

    crate::println!("[task] Preemptive multitasking initialized (task #0 = shell)");
}

/// Spawn a new task with the given name and function.
pub fn spawn(name: &str, f: fn()) -> usize {
    let id = unsafe {
        let id = NEXT_TASK_ID;
        NEXT_TASK_ID += 1;
        id
    };

    // Allocate a 16 KiB stack
    let stack = alloc::vec![0u8; TASK_STACK_SIZE];

    // Set up the initial context
    // Stack grows down; align to 16 bytes
    let stack_top = (stack.as_ptr() as usize + TASK_STACK_SIZE) & !0xF;
    let ctx = TaskContext {
        ra: task_entry as *const () as usize,
        sp: stack_top,
        s: {
            let mut s = [0usize; 12];
            s[0] = f as usize; // s0 = function pointer
            s
        },
    };

    // Create graph node
    let tasks_node_id = unsafe { TASKS_NODE_ID };
    let g = graph::get_mut();
    let node_id = g.create_node(NodeType::System, name);
    g.add_edge(tasks_node_id, "child", node_id);

    // Set initial content
    if let Some(node) = g.get_node_mut(node_id) {
        let info = alloc::format!("state: ready\nstack: {} bytes", TASK_STACK_SIZE);
        node.content = info.into_bytes();
    }

    let task = Task {
        id,
        name: String::from(name),
        state: TaskState::Ready,
        context: ctx,
        stack,
        graph_node_id: node_id,
        preempt_count: 0,
    };

    tasks_mut().push(task);
    id
}

/// Mark the currently running task as Done.
pub fn current_task_done() {
    let current_idx = unsafe { CURRENT_TASK };
    let tasks = tasks_mut();
    let task = &mut tasks[current_idx];
    task.state = TaskState::Done;
    // Update graph node
    let node_id = task.graph_node_id;
    let g = graph::get_mut();
    if let Some(node) = g.get_node_mut(node_id) {
        let info = alloc::format!("state: done\nstack: {} bytes", TASK_STACK_SIZE);
        node.content = info.into_bytes();
    }
}

/// Cooperatively yield the CPU to the next ready task.
pub fn yield_now() {
    let tasks = tasks_mut();
    let n = tasks.len();
    if n <= 1 {
        return;
    }

    let current_idx = unsafe { CURRENT_TASK };

    // Find the next ready task (round-robin)
    let mut next_idx = None;
    for i in 1..n {
        let idx = (current_idx + i) % n;
        if tasks[idx].state == TaskState::Ready {
            next_idx = Some(idx);
            break;
        }
    }

    let next_idx = match next_idx {
        Some(idx) => idx,
        None => return, // No other ready tasks
    };

    // Update states
    if tasks[current_idx].state == TaskState::Running {
        tasks[current_idx].state = TaskState::Ready;
    }
    tasks[next_idx].state = TaskState::Running;

    // Get raw pointers to contexts before the switch
    let old_ctx = &mut tasks[current_idx].context as *mut TaskContext;
    let new_ctx = &tasks[next_idx].context as *const TaskContext;

    unsafe {
        CURRENT_TASK = next_idx;
        switch_context(old_ctx, new_ctx);
    }
}

/// Kill a task by ID (mark it as Done so it won't be scheduled again).
pub fn kill(id: usize) -> bool {
    if id == 0 {
        return false; // Can't kill the shell
    }
    let tasks = tasks_mut();
    if let Some(task) = tasks.iter_mut().find(|t| t.id == id) {
        if task.state == TaskState::Done {
            return false;
        }
        task.state = TaskState::Done;
        // Update graph node
        let g = graph::get_mut();
        if let Some(node) = g.get_node_mut(task.graph_node_id) {
            let info = alloc::format!("state: done (killed)\nstack: {} bytes", TASK_STACK_SIZE);
            node.content = info.into_bytes();
        }
        true
    } else {
        false
    }
}

/// List all tasks. Returns (id, name, state, preempt_count) tuples.
pub fn list() -> Vec<(usize, String, TaskState, usize)> {
    tasks()
        .iter()
        .map(|t| (t.id, t.name.clone(), t.state, t.preempt_count))
        .collect()
}

/// Refresh all task graph nodes with current state.
pub fn refresh_task_nodes() {
    let tasks = tasks();
    for task in tasks.iter() {
        let g = graph::get_mut();
        if let Some(node) = g.get_node_mut(task.graph_node_id) {
            let info = alloc::format!(
                "state: {}\nstack: {} bytes\npreemptions: {}",
                task.state,
                if task.id == 0 { 0 } else { TASK_STACK_SIZE },
                task.preempt_count
            );
            node.content = info.into_bytes();
        }
    }
}

// ---------------------------------------------------------------------------
// Preemptive yield — called from the timer interrupt handler
// ---------------------------------------------------------------------------

/// Bit 1 of `sstatus` — Supervisor Interrupt Enable.
const SSTATUS_SIE: usize = 1 << 1;

/// Called from the timer interrupt handler to preemptively switch tasks.
/// Re-enables interrupts before switching so the next task can also be
/// preempted. When we're switched back, disables interrupts again since
/// we're still inside the trap handler (assembly will `sret`).
pub fn preemptive_yield() {
    let tasks = tasks_mut();
    let n = tasks.len();
    if n <= 1 {
        return;
    }

    let current_idx = unsafe { CURRENT_TASK };

    // Find the next ready task (round-robin)
    let mut next_idx = None;
    for i in 1..n {
        let idx = (current_idx + i) % n;
        if tasks[idx].state == TaskState::Ready {
            next_idx = Some(idx);
            break;
        }
    }

    let next_idx = match next_idx {
        Some(idx) => idx,
        None => return, // No other ready tasks
    };

    // Increment preemption count for the current task
    tasks[current_idx].preempt_count += 1;

    // Update graph node for current task to show preemption
    let cur_node_id = tasks[current_idx].graph_node_id;
    let cur_preempt = tasks[current_idx].preempt_count;
    let cur_id = tasks[current_idx].id;
    let g = graph::get_mut();
    if let Some(node) = g.get_node_mut(cur_node_id) {
        let info = alloc::format!(
            "state: preempted\nstack: {} bytes\npreemptions: {}",
            if cur_id == 0 { 0 } else { TASK_STACK_SIZE },
            cur_preempt
        );
        node.content = info.into_bytes();
    }

    // Update states
    if tasks[current_idx].state == TaskState::Running {
        tasks[current_idx].state = TaskState::Ready;
    }
    tasks[next_idx].state = TaskState::Running;

    // Get raw pointers to contexts before the switch
    let old_ctx = &mut tasks[current_idx].context as *mut TaskContext;
    let new_ctx = &tasks[next_idx].context as *const TaskContext;

    unsafe {
        CURRENT_TASK = next_idx;

        // Re-enable supervisor interrupts before switching so the next task
        // can be preempted. We're about to switch away, so this is safe.
        let sstatus = arch::read_sstatus();
        arch::write_sstatus(sstatus | SSTATUS_SIE);

        switch_context(old_ctx, new_ctx);

        // We've been switched back to. We're still inside the trap handler
        // path, so disable interrupts — the assembly `sret` will restore
        // sstatus.SIE from sstatus.SPIE.
        let sstatus = arch::read_sstatus();
        arch::write_sstatus(sstatus & !SSTATUS_SIE);
    }
}

// ---------------------------------------------------------------------------
// Demo tasks
// ---------------------------------------------------------------------------

/// A counter task that prints 10 iterations, yielding between each.
pub fn demo_counter() {
    for i in 1..=10 {
        crate::println!("Task 'counter' iteration {}", i);
        yield_now();
    }
}

/// A fibonacci task that computes fib numbers, yielding between each.
pub fn demo_fibonacci() {
    let mut a: u64 = 0;
    let mut b: u64 = 1;
    for i in 1..=10 {
        let fib = b;
        crate::println!("Task 'fibonacci': fib({}) = {}", i, fib);
        let next = a + b;
        a = b;
        b = next;
        yield_now();
    }
}

/// A busy-loop task that does NOT call yield_now(). It will only make
/// progress when preempted by the timer interrupt. Proves preemptive
/// multitasking is working.
pub fn demo_busyloop() {
    let mut counter: u64 = 0;
    let mut last_print: u64 = 0;
    loop {
        counter = counter.wrapping_add(1);
        // Print every ~10 million iterations (roughly every few preemptions)
        if counter.wrapping_sub(last_print) >= 10_000_000 {
            crate::println!(
                "[busyloop] counter={} (preempted {} times)",
                counter,
                preempt_count_current()
            );
            last_print = counter;
            // Stop after printing 10 times to not run forever
            if counter >= 100_000_000 {
                crate::println!("[busyloop] done!");
                return;
            }
        }
    }
}

/// Get the preemption count for the currently running task.
pub fn preempt_count_current() -> usize {
    let current_idx = unsafe { CURRENT_TASK };
    tasks()[current_idx].preempt_count
}
