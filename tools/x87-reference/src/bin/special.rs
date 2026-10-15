use std::arch::asm;
fn main() {
    for op in 0..3 {
        for bits in [
            0x7FF8000000000001_u64,
            0x7FF0000000000001,
            0x7FF0000000000000,
        ] {
            let x = f64::from_bits(bits);
            let mut a = [0_u8; 10];
            let mut b = [0_u8; 10];
            let mut sw = 0_u16;
            unsafe {
                asm!("fninit","fld qword ptr [{x}]",x=in(reg)&x);
                match op {
                    0 => asm!("fptan"),
                    1 => asm!("fsincos"),
                    _ => asm!("fxtract"),
                }
                asm!("fnstsw [{sw}]","fstp tbyte ptr [{a}]","fstp tbyte ptr [{b}]",sw=in(reg)&mut sw,a=in(reg)a.as_mut_ptr(),b=in(reg)b.as_mut_ptr());
            }
            println!("{op} {bits:016X} SW{sw:04X} A{a:02X?} B{b:02X?}");
        }
    }
}
