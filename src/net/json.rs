/// Tiny no_std JSON encoder for Helios.
///
/// Just enough to serialize the graph as a browser-friendly document.
/// Not a general-purpose JSON lib — no parsing, no big decimals, no schema.
/// The philosophy: build valid JSON by composition of small writer helpers.
///
/// Every writer appends to an `alloc::string::String` and returns `()`.
/// Callers are responsible for comma placement; we provide `ArrayBuilder`
/// and `ObjectBuilder` wrappers that handle that for the common case.

use alloc::string::String;

/// Escape a string into a JSON string literal (including surrounding quotes).
pub fn escape_into(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000C}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                // Other control chars: \uXXXX
                let code = c as u32;
                out.push_str("\\u");
                for shift in [12u32, 8, 4, 0] {
                    let nib = (code >> shift) & 0xF;
                    let ch = if nib < 10 { (b'0' + nib as u8) as char } else { (b'a' + (nib - 10) as u8) as char };
                    out.push(ch);
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Append a bare number (u64).
pub fn num_u64(out: &mut String, n: u64) {
    use core::fmt::Write;
    let _ = write!(out, "{}", n);
}

/// Append a bare number (usize).
pub fn num_usize(out: &mut String, n: usize) {
    use core::fmt::Write;
    let _ = write!(out, "{}", n);
}

/// Append a bool.
pub fn boolean(out: &mut String, b: bool) {
    out.push_str(if b { "true" } else { "false" });
}

/// Append the literal null.
pub fn null(out: &mut String) {
    out.push_str("null");
}

/// Object builder that handles commas between fields.
pub struct ObjectBuilder<'a> {
    out: &'a mut String,
    first: bool,
}

impl<'a> ObjectBuilder<'a> {
    pub fn new(out: &'a mut String) -> Self {
        out.push('{');
        ObjectBuilder { out, first: true }
    }

    fn prep_key(&mut self, key: &str) {
        if !self.first {
            self.out.push(',');
        }
        self.first = false;
        escape_into(self.out, key);
        self.out.push(':');
    }

    pub fn str_field(&mut self, key: &str, value: &str) {
        self.prep_key(key);
        escape_into(self.out, value);
    }

    pub fn u64_field(&mut self, key: &str, value: u64) {
        self.prep_key(key);
        num_u64(self.out, value);
    }

    pub fn usize_field(&mut self, key: &str, value: usize) {
        self.prep_key(key);
        num_usize(self.out, value);
    }

    pub fn bool_field(&mut self, key: &str, value: bool) {
        self.prep_key(key);
        boolean(self.out, value);
    }

    /// Emit a key, then hand the caller a mutable String ref to write
    /// whatever JSON they want (object, array, etc.).
    pub fn raw_field<F: FnOnce(&mut String)>(&mut self, key: &str, f: F) {
        self.prep_key(key);
        f(self.out);
    }

    pub fn finish(self) {
        self.out.push('}');
    }
}

/// Array builder that handles commas between items.
pub struct ArrayBuilder<'a> {
    out: &'a mut String,
    first: bool,
}

impl<'a> ArrayBuilder<'a> {
    pub fn new(out: &'a mut String) -> Self {
        out.push('[');
        ArrayBuilder { out, first: true }
    }

    fn prep(&mut self) {
        if !self.first {
            self.out.push(',');
        }
        self.first = false;
    }

    pub fn str_item(&mut self, value: &str) {
        self.prep();
        escape_into(self.out, value);
    }

    pub fn u64_item(&mut self, value: u64) {
        self.prep();
        num_u64(self.out, value);
    }

    /// Hand the caller a mutable String ref to write whatever JSON item.
    pub fn raw_item<F: FnOnce(&mut String)>(&mut self, f: F) {
        self.prep();
        f(self.out);
    }

    pub fn finish(self) {
        self.out.push(']');
    }
}
