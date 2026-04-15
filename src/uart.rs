/// NS16550A UART driver for QEMU virt machine.
/// Base address: 0x1000_0000

const UART_BASE: usize = 0x1000_0000;

// Register offsets
const THR: usize = 0; // Transmit Holding Register (write)
const RBR: usize = 0; // Receive Buffer Register (read)
const IER: usize = 1; // Interrupt Enable Register
const FCR: usize = 2; // FIFO Control Register
const LCR: usize = 3; // Line Control Register
const LSR: usize = 5; // Line Status Register

const LSR_TX_EMPTY: u8 = 0x20;
const LSR_DATA_READY: u8 = 0x01;

pub struct Uart {
    base: usize,
}

impl Uart {
    pub const fn new(base: usize) -> Self {
        Self { base }
    }

    /// Initialize the UART: 8-N-1, enable FIFO
    pub fn init(&self) {
        unsafe {
            let base = self.base as *mut u8;
            // Disable interrupts
            base.add(IER).write_volatile(0x00);
            // Enable FIFO, clear them, 14-byte threshold
            base.add(FCR).write_volatile(0xC7);
            // 8 bits, no parity, one stop bit (8-N-1)
            base.add(LCR).write_volatile(0x03);
            // Enable interrupts (receive)
            base.add(IER).write_volatile(0x01);
        }
    }

    /// Write a single byte, waiting for the transmit buffer to be empty.
    pub fn putc(&self, c: u8) {
        unsafe {
            let base = self.base as *mut u8;
            // Wait until THR is empty
            while base.add(LSR).read_volatile() & LSR_TX_EMPTY == 0 {
                core::hint::spin_loop();
            }
            base.add(THR).write_volatile(c);
        }
    }

    /// Check if data is available in the receive buffer.
    pub fn has_data(&self) -> bool {
        unsafe {
            let base = self.base as *mut u8;
            base.add(LSR).read_volatile() & LSR_DATA_READY != 0
        }
    }

    /// Non-blocking read of a single byte from the receive buffer.
    pub fn getc(&self) -> Option<u8> {
        if self.has_data() {
            unsafe {
                let base = self.base as *mut u8;
                Some(base.add(RBR).read_volatile())
            }
        } else {
            None
        }
    }

    /// Write a string
    pub fn puts(&self, s: &str) {
        for byte in s.bytes() {
            if byte == b'\n' {
                self.putc(b'\r');
            }
            self.putc(byte);
        }
    }
}

/// Global UART instance
static UART: Uart = Uart::new(UART_BASE);

pub fn init() {
    UART.init();
}

pub fn puts(s: &str) {
    UART.puts(s);
}

pub fn putc(c: u8) {
    UART.putc(c);
}

pub fn has_data() -> bool {
    UART.has_data()
}

pub fn getc() -> Option<u8> {
    UART.getc()
}

/// Simple print macro
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {
        $crate::uart::_print(format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => {
        $crate::print!("{}\n", format_args!($($arg)*))
    };
}

use core::fmt::{self, Write};

struct UartWriter;

impl Write for UartWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        puts(s);
        // Dual-output: also write to framebuffer console if active
        crate::console::write_str(s);
        Ok(())
    }
}

pub fn _print(args: fmt::Arguments) {
    UartWriter.write_fmt(args).unwrap();
}
