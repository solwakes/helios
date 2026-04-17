//! Simple byte-oriented I/O for Helios tasks.
//!
//! All output currently goes through `SYS_PRINT`, which the kernel
//! sends straight to the UART. There's no "stdout" concept in the
//! kernel — a task just prints and the kernel decides where (for M31
//! that's always the serial console).

use crate::sys;
use core::fmt;

/// Write `s` to the task's default output channel via `SYS_PRINT`.
///
/// On failure (the kernel only returns negative from `SYS_PRINT` on a
/// bad buffer, which can't happen for a valid `&str` in our VA window)
/// the call silently drops.
pub fn print(s: &str) {
    // SYS_PRINT writes up to 4096 bytes per call; we chunk anything
    // larger so `write!(...)` into a growing `String` still works.
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let take = core::cmp::min(bytes.len() - i, 4096);
        let _ = unsafe {
            sys::syscall2(
                sys::SYS_PRINT,
                bytes.as_ptr().wrapping_add(i) as usize,
                take,
            )
        };
        i += take;
    }
}

/// Write `s` followed by a newline.
pub fn println(s: &str) {
    print(s);
    print("\n");
}

/// An empty type implementing [`core::fmt::Write`] — lets you use
/// `write!`/`writeln!` directly against the kernel's print syscall.
///
/// ```ignore
/// use core::fmt::Write;
/// let _ = writeln!(helios_std::io::Stdout, "hello #{}", 42);
/// ```
pub struct Stdout;

impl fmt::Write for Stdout {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        print(s);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Macros
// ---------------------------------------------------------------------------

/// `print!`-like macro that formats its arguments and writes them via
/// `SYS_PRINT`. Requires `alloc` or uses direct `write!` on
/// [`Stdout`]; the latter is what this emits (no alloc).
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {{
        use core::fmt::Write as _;
        let _ = core::write!(&mut $crate::io::Stdout, $($arg)*);
    }};
}

/// `println!`-like macro — prints + newline.
#[macro_export]
macro_rules! println {
    () => { $crate::io::print("\n") };
    ($($arg:tt)*) => {{
        use core::fmt::Write as _;
        let _ = core::writeln!(&mut $crate::io::Stdout, $($arg)*);
    }};
}
