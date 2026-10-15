use std::arch::asm;
fn main() {
    println!("# Native x86-64 x87; CW OP Xbits Ybits result-significand result-exponent SW");
    for pc in [0, 0x200, 0x300] {
        for rc in 0..4_u16 {
            let cw = 0x7F | pc | (rc << 10);
            for op in 0..5 {
                for x in [
                    -7_f64, -3., -1., -0.5, -0.1, -1e-20, -1e-200, -0., 0., 1e-200, 1e-20, 0.1,
                    0.5, 1., 3., 7., 1e10, 1e18,
                ] {
                    for y in [1_f64, -2.5] {
                        if op == 4 && x <= -1. {
                            continue;
                        }
                        let mut saved = 0_u16;
                        let mut status = 0_u16;
                        let mut result = [0_u8; 10];
                        unsafe {
                            asm!("fnstcw [{saved}]", "fnclex", "fldcw [{cw}]", "fld qword ptr [{y}]", "fld qword ptr [{x}]",saved=in(reg)&mut saved,cw=in(reg)&cw,y=in(reg)&y,x=in(reg)&x);
                            match op {
                                0 => asm!("fsin"),
                                1 => asm!("fcos"),
                                2 => asm!("fptan", "fstp st(0)"),
                                3 => asm!("fpatan"),
                                _ => asm!("fyl2xp1"),
                            }
                            asm!("fnstsw [{status}]", "fstp tbyte ptr [{result}]",status=in(reg)&mut status,result=in(reg)result.as_mut_ptr());
                            if op < 3 {
                                asm!("fstp st(0)");
                            }
                            asm!("fnclex", "fldcw [{saved}]",saved=in(reg)&saved);
                        }
                        println!(
                            "{cw:04X} {op:02X} {:016X} {:016X} {:016X} {:04X} {status:04X}",
                            x.to_bits(),
                            y.to_bits(),
                            u64::from_le_bytes(result[..8].try_into().unwrap()),
                            u16::from_le_bytes(result[8..].try_into().unwrap())
                        );
                    }
                }
            }
        }
    }
}
