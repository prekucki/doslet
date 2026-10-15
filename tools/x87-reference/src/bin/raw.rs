use std::arch::asm;
fn main() {
    for (mantissa, exponent) in [
        (0_u64, 0_u16),
        (1, 0),
        (1 << 63, 0),
        (u64::MAX, 0),
        (1 << 63, 1),
        (u64::MAX, 0x7FFE),
        (1 << 63, 0x7FFF),
        (0x8000000000000001, 0x7FFF),
        (0xC000000000000001, 0x7FFF),
        (0, 0x3FFF),
    ] {
        let mut input = [0_u8; 10];
        input[..8].copy_from_slice(&mantissa.to_le_bytes());
        input[8..].copy_from_slice(&exponent.to_le_bytes());
        let mut result = [0_u8; 10];
        let mut sw = 0_u16;
        let mut env = [0_u8; 28];
        unsafe {
            asm!("fninit","fld tbyte ptr [{input}]","fnstsw [{sw}]","fnstenv [{env}]","fstp tbyte ptr [{result}]",input=in(reg)input.as_ptr(),sw=in(reg)&mut sw,env=in(reg)env.as_mut_ptr(),result=in(reg)result.as_mut_ptr());
        }
        println!(
            "{mantissa:016X} {exponent:04X} => {:016X} {:04X} SW {sw:04X} TW {:04X}",
            u64::from_le_bytes(result[..8].try_into().unwrap()),
            u16::from_le_bytes(result[8..].try_into().unwrap()),
            u16::from_le_bytes(env[8..10].try_into().unwrap())
        );
    }
}
