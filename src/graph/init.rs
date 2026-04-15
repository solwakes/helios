/// Bootstrap the initial graph structure with system nodes.

use super::{NodeType, get_mut};

pub fn bootstrap() {
    let g = get_mut();

    // ID 1: root
    let root_id = g.create_node(NodeType::System, "root");

    // ID 2: system
    let sys_id = g.create_node(NodeType::System, "system");

    // ID 3: devices
    let dev_id = g.create_node(NodeType::Directory, "devices");

    // root -> system, root -> devices
    g.add_edge(root_id, "child", sys_id);
    g.add_edge(root_id, "child", dev_id);

    // ID 4: uart0
    let uart_id = g.create_node(NodeType::System, "uart0");
    if let Some(node) = g.get_node_mut(uart_id) {
        node.content = alloc::vec::Vec::from("NS16550A UART @ 0x10000000".as_bytes());
    }

    // ID 5: framebuffer0
    let fb_id = g.create_node(NodeType::System, "framebuffer0");
    if let Some(node) = g.get_node_mut(fb_id) {
        node.content = alloc::vec::Vec::from("ramfb display device".as_bytes());
    }

    // devices -> uart0, devices -> framebuffer0
    g.add_edge(dev_id, "child", uart_id);
    g.add_edge(dev_id, "child", fb_id);

    // ID 6: memory
    let mem_id = g.create_node(NodeType::System, "memory");

    // ID 7: timer
    let timer_id = g.create_node(NodeType::System, "timer");

    // ID 8: cpu
    let cpu_id = g.create_node(NodeType::System, "cpu");

    // system -> memory, system -> timer, system -> cpu
    g.add_edge(sys_id, "child", mem_id);
    g.add_edge(sys_id, "child", timer_id);
    g.add_edge(sys_id, "child", cpu_id);

    // ID 9: dashboard (computed reactive node)
    // Create a computed "dashboard" node under root
    let dash_id = g.create_node(NodeType::Computed, "dashboard");
    g.add_edge(root_id, "child", dash_id);
    if let Some(node) = g.get_node_mut(dash_id) {
        node.content = alloc::vec::Vec::from(
            "$template{Uptime: $uptime | Heap: $mem | Graph: $graph}".as_bytes()
        );
    }

    // Populate all system nodes with initial live data
    super::live::refresh_system_nodes();
}
