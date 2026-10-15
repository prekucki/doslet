use std::arch::asm;
fn main() {
    for rc in 0..4_u16 {
        let cw = 0x37F | (rc << 10);
        for single in [true, false] {
            for (mantissa, exponent) in [
                (1_u64, 0_u16),
                (1 << 63, 1),
                (1 << 63, 0x3FFF),
                (u64::MAX, 0x7FFE),
                (u64::MAX, 0x3FFF - 127),
                (0xFFFF_FF00_0000_0000, 0x3FFF - 127),
                (0x8000_0080_0000_0000, 0x3FFF),
                (u64::MAX, 0x3FFF - 1023),
                (u64::MAX, 0x3FFF + 127),
                (u64::MAX, 0x3FFF + 1023),
            ] {
                for sign in [0, 0x8000] {
                    let mut input = [0_u8; 10];
                    input[..8].copy_from_slice(&mantissa.to_le_bytes());
                    input[8..].copy_from_slice(&(exponent | sign).to_le_bytes());
                    let mut saved = 0_u16;
                    let mut sw = 0_u16;
                    let mut result = [0_u8; 8];
                    unsafe {
                        asm!("fnstcw [{saved}]","fnclex","fldcw [{cw}]","fld tbyte ptr [{input}]", saved=in(reg)&mut saved,cw=in(reg)&cw,input=in(reg)input.as_ptr());
                        if single {
                            asm!("fstp dword ptr [{result}]",result=in(reg)result.as_mut_ptr());
                        } else {
                            asm!("fstp qword ptr [{result}]",result=in(reg)result.as_mut_ptr());
                        }
                        asm!("fnstsw [{sw}]","fnclex","fldcw [{saved}]",sw=in(reg)&mut sw,saved=in(reg)&saved);
                    }
                    let result = if single {
                        f64::from(f32::from_le_bytes(result[..4].try_into().unwrap())).to_bits()
                    } else {
                        u64::from_le_bytes(result)
                    };
                    println!(
                        "{cw:04X} {:02X} {mantissa:016X} {:04X} {result:016X} {sw:04X}",
                        u8::from(single),
                        exponent | sign
                    );
                }
            }
        }
    }
}
