use std::arch::asm;
fn main() {
    println!(
        "# Captured native x86-64 x87; CW OP Xbits Ybits result-significand result-exponent SW"
    );
    for pc in [0, 0x200, 0x300] {
        for rc in 0..4_u16 {
            let cw = 0x7F | pc | (rc << 10);
            for op in 0..3 {
                for (x, y) in [
                    (0., 1_f64),
                    (-0., 1.),
                    (1., 1.),
                    (2., 3.),
                    (3., 2.),
                    (7., 2.),
                    (-7., 2.),
                    (7., -2.),
                    (1e200, 3.),
                    (1e-200, 3.),
                    (0.75, 1.25),
                    (f64::MAX, 1.1),
                    (f64::from_bits(1), 2.),
                ] {
                    if op == 0 && x < 0. {
                        continue;
                    }
                    let mut saved = 0_u16;
                    let mut status = 0_u16;
                    let mut result = [0_u8; 10];
                    unsafe {
                        asm!("fnstcw [{saved}]", "fnclex", "fldcw [{cw}]", "fld qword ptr [{y}]", "fld qword ptr [{x}]",saved=in(reg)&mut saved,cw=in(reg)&cw,y=in(reg)&y,x=in(reg)&x);
                        match op {
                            0 => asm!("fsqrt"),
                            1 => {
                                asm!("2:", "fprem", "fnstsw ax", "test ax, 0x400", "jnz 2b", out("ax") _)
                            }
                            _ => {
                                asm!("2:", "fprem1", "fnstsw ax", "test ax, 0x400", "jnz 2b", out("ax") _)
                            }
                        }
                        asm!("fnstsw [{status}]", "fstp tbyte ptr [{result}]", "fstp st(0)", "fnclex", "fldcw [{saved}]",status=in(reg)&mut status,result=in(reg)result.as_mut_ptr(),saved=in(reg)&saved);
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
