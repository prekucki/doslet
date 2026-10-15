use std::arch::asm;
fn main() {
    for rc in 0..4_u16 {
        let cw = 0x37F | (rc << 10);
        for (mantissa, exponent) in [
            (1_u64, 0_u16),
            (1 << 63, 1),
            (1 << 63, 0x3FFF),
            (u64::MAX, 0x7FFE),
        ] {
            for sign in [0, 0x8000] {
                for scale in [-65536_f64, -16446., -64., -1., 1., 64., 16446., 65536.] {
                    let mut input = [0_u8; 10];
                    input[..8].copy_from_slice(&mantissa.to_le_bytes());
                    input[8..].copy_from_slice(&(exponent | sign).to_le_bytes());
                    let mut saved = 0_u16;
                    let mut sw = 0_u16;
                    let mut result = [0_u8; 10];
                    unsafe {
                        asm!("fnstcw [{saved}]","fnclex","fldcw [{cw}]","fld qword ptr [{scale}]","fld tbyte ptr [{input}]","fscale","fnstsw [{sw}]","fstp tbyte ptr [{result}]","fstp st(0)","fnclex","fldcw [{saved}]", saved=in(reg)&mut saved,cw=in(reg)&cw,scale=in(reg)&scale,input=in(reg)input.as_ptr(),sw=in(reg)&mut sw,result=in(reg)result.as_mut_ptr());
                    }
                    println!(
                        "{cw:04X} {mantissa:016X} {:04X} {:016X} {:016X} {:04X} {sw:04X}",
                        exponent | sign,
                        scale.to_bits(),
                        u64::from_le_bytes(result[..8].try_into().unwrap()),
                        u16::from_le_bytes(result[8..].try_into().unwrap())
                    );
                }
            }
        }
    }
}
