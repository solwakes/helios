/// Manual implementations of memset/memcpy/memmove/memcmp/bcmp.
/// These replace the compiler_builtins versions which hang on RISC-V 64.
///
/// CRITICAL: All loops use volatile operations to prevent LLVM from
/// recognising the loop pattern and replacing it with a recursive
/// call back to the very function we're implementing.

#[no_mangle]
#[inline(never)]
pub unsafe extern "C" fn memset(dest: *mut u8, c: i32, n: usize) -> *mut u8 {
    let c = c as u8;
    let mut i = 0usize;
    while i < n {
        core::ptr::write_volatile(dest.add(i), c);
        i += 1;
    }
    dest
}

#[no_mangle]
#[inline(never)]
pub unsafe extern "C" fn memcpy(dest: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    let mut i = 0usize;
    while i < n {
        core::ptr::write_volatile(dest.add(i), core::ptr::read_volatile(src.add(i)));
        i += 1;
    }
    dest
}

#[no_mangle]
#[inline(never)]
pub unsafe extern "C" fn memmove(dest: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    if (dest as usize) <= (src as usize) || (dest as usize) >= (src as usize) + n {
        // Non-overlapping or forward copy is safe
        memcpy(dest, src, n)
    } else {
        // Backward copy for overlapping regions
        let mut i = n;
        while i > 0 {
            i -= 1;
            core::ptr::write_volatile(dest.add(i), core::ptr::read_volatile(src.add(i)));
        }
        dest
    }
}

#[no_mangle]
#[inline(never)]
pub unsafe extern "C" fn memcmp(s1: *const u8, s2: *const u8, n: usize) -> i32 {
    let mut i = 0usize;
    while i < n {
        let a = core::ptr::read_volatile(s1.add(i));
        let b = core::ptr::read_volatile(s2.add(i));
        if a != b {
            return (a as i32) - (b as i32);
        }
        i += 1;
    }
    0
}

#[no_mangle]
#[inline(never)]
pub unsafe extern "C" fn bcmp(s1: *const u8, s2: *const u8, n: usize) -> i32 {
    memcmp(s1, s2, n)
}
