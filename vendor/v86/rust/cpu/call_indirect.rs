static mut ENTRIES: u32 = 0;
#[no_mangle]
pub fn doslet_jit_entries() -> u32 { unsafe { ENTRIES } }
#[no_mangle]
pub fn call_indirect1(f: fn(u16), x: u16) { unsafe { ENTRIES = ENTRIES.wrapping_add(1); } f(x); }
