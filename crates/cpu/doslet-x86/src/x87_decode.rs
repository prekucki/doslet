//! Complete documented 80387 D8–DF encoding map, independent of execution.
//! Reserved/undocumented aliases are deliberately not accepted. P6 and SSE3
//! encodings are identified separately; they do not extend the emulated ISA.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mnemonic {
    Fadd,
    Fmul,
    Fcom,
    Fcomp,
    Fsub,
    Fsubr,
    Fdiv,
    Fdivr,
    Fiadd,
    Fimul,
    Ficom,
    Ficomp,
    Fisub,
    Fisubr,
    Fidiv,
    Fidivr,
    Faddp,
    Fmulp,
    Fsubrp,
    Fsubp,
    Fdivrp,
    Fdivp,
    Fld,
    Fst,
    Fstp,
    Fild,
    Fist,
    Fistp,
    Fbld,
    Fbstp,
    Fldenv,
    Fldcw,
    Fnstenv,
    Fnstcw,
    Frstor,
    Fnsave,
    Fnstsw,
    Fxch,
    Fnop,
    Fchs,
    Fabs,
    Ftst,
    Fxam,
    Fld1,
    Fldl2t,
    Fldl2e,
    Fldpi,
    Fldlg2,
    Fldln2,
    Fldz,
    F2xm1,
    Fyl2x,
    Fptan,
    Fpatan,
    Fxtract,
    Fprem1,
    Fdecstp,
    Fincstp,
    Fprem,
    Fyl2xp1,
    Fsqrt,
    Fsincos,
    Frndint,
    Fscale,
    Fsin,
    Fcos,
    Fucompp,
    Fneni,
    Fndisi,
    Fnclex,
    Fninit,
    Fnsetpm,
    Ffree,
    Fucom,
    Fucomp,
    Fcompp,
    Fcmovb,
    Fcmove,
    Fcmovbe,
    Fcmovu,
    Fcmovnb,
    Fcmovne,
    Fcmovnbe,
    Fcmovnu,
    Fucomi,
    Fcomi,
    Fucomip,
    Fcomip,
    Fisttp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryFormat {
    Real32,
    Real64,
    Real80,
    Int16,
    Int32,
    Int64,
    Bcd80,
    Control16,
    Status16,
    Environment,
    State,
}
impl MemoryFormat {
    /// Operand-size attribute selects the environment layout, independently
    /// of the address-size attribute used to locate the operand.
    pub const fn bytes(self, operand_32: bool) -> u8 {
        match self {
            Self::Real32 | Self::Int32 => 4,
            Self::Real64 | Self::Int64 => 8,
            Self::Real80 | Self::Bcd80 => 10,
            Self::Int16 | Self::Control16 | Self::Status16 => 2,
            Self::Environment => {
                if operand_32 {
                    28
                } else {
                    14
                }
            }
            Self::State => {
                if operand_32 {
                    108
                } else {
                    94
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operands {
    None,
    Ax,
    /// ST(0), ST(i), in Intel operand order.
    TopStack(u8),
    /// ST(i), ST(0), including pop forms.
    StackTop(u8),
    Stack(u8),
    Memory(MemoryFormat),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Instruction {
    pub mnemonic: Mnemonic,
    pub operands: Operands,
}
impl Instruction {
    pub const fn pops(self) -> u8 {
        use Mnemonic::*;
        match self.mnemonic {
            Fcompp | Fucompp => 2,
            Fcomp | Ficomp | Faddp | Fmulp | Fsubrp | Fsubp | Fdivrp | Fdivp | Fstp | Fistp
            | Fbstp | Fucomp | Fucomip | Fcomip | Fisttp | Fyl2x | Fpatan | Fyl2xp1 => 1,
            _ => 0,
        }
    }
    /// Opcode support only. Numeric ranges and exception delivery have their
    /// own explicit execution errors; this is not a full IEEE-754 claim.
    pub const fn implemented(self) -> bool {
        use Mnemonic::*;
        !matches!(
            self.mnemonic,
            Fcmovb
                | Fcmove
                | Fcmovbe
                | Fcmovu
                | Fcmovnb
                | Fcmovne
                | Fcmovnbe
                | Fcmovnu
                | Fucomi
                | Fcomi
                | Fucomip
                | Fcomip
                | Fisttp
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    I387(Instruction),
    /// P6: conditional moves/compare-to-EFLAGS. SSE3: FISTTP.
    LaterIsa(Instruction),
    Reserved,
    NotX87,
}

/// Decode opcode plus ModR/M without fetching displacement/SIB or touching
/// architectural state. WAIT (9Bh) remains a separate CPU instruction.
pub const fn decode(opcode: u8, modrm: u8) -> Encoding {
    use MemoryFormat::*;
    use Mnemonic::*;
    use Operands::*;
    if opcode < 0xD8 || opcode > 0xDF {
        return Encoding::NotX87;
    }
    let group = (modrm >> 3) & 7;
    let index = modrm & 7;
    let mut later = false;
    let (mnemonic, operands) = if modrm < 0xC0 {
        let format = match opcode {
            0xD8 => Real32,
            0xDA => Int32,
            0xDC => Real64,
            _ => Int16,
        };
        match opcode {
            0xD8 | 0xDA | 0xDC | 0xDE => {
                let mnemonic = if opcode == 0xDA || opcode == 0xDE {
                    [Fiadd, Fimul, Ficom, Ficomp, Fisub, Fisubr, Fidiv, Fidivr][group as usize]
                } else {
                    [Fadd, Fmul, Fcom, Fcomp, Fsub, Fsubr, Fdiv, Fdivr][group as usize]
                };
                (mnemonic, Memory(format))
            }
            0xD9 => match group {
                0 => (Fld, Memory(Real32)),
                2 => (Fst, Memory(Real32)),
                3 => (Fstp, Memory(Real32)),
                4 => (Fldenv, Memory(Environment)),
                5 => (Fldcw, Memory(Control16)),
                6 => (Fnstenv, Memory(Environment)),
                7 => (Fnstcw, Memory(Control16)),
                _ => return Encoding::Reserved,
            },
            0xDB | 0xDD | 0xDF => {
                let format = match opcode {
                    0xDB => Int32,
                    0xDD => Real64,
                    _ => Int16,
                };
                match group {
                    0 => (if opcode == 0xDD { Fld } else { Fild }, Memory(format)),
                    1 => {
                        later = true;
                        (Fisttp, Memory(if opcode == 0xDD { Int64 } else { format }))
                    }
                    2 => (if opcode == 0xDD { Fst } else { Fist }, Memory(format)),
                    3 => (if opcode == 0xDD { Fstp } else { Fistp }, Memory(format)),
                    4 if opcode == 0xDD => (Frstor, Memory(State)),
                    6 if opcode == 0xDD => (Fnsave, Memory(State)),
                    7 if opcode == 0xDD => (Fnstsw, Memory(Status16)),
                    5 if opcode == 0xDB => (Fld, Memory(Real80)),
                    7 if opcode == 0xDB => (Fstp, Memory(Real80)),
                    4 if opcode == 0xDF => (Fbld, Memory(Bcd80)),
                    5 if opcode == 0xDF => (Fild, Memory(Int64)),
                    6 if opcode == 0xDF => (Fbstp, Memory(Bcd80)),
                    7 if opcode == 0xDF => (Fistp, Memory(Int64)),
                    _ => return Encoding::Reserved,
                }
            }
            _ => return Encoding::Reserved,
        }
    } else {
        match opcode {
            0xD8 => {
                let mnemonic = [Fadd, Fmul, Fcom, Fcomp, Fsub, Fsubr, Fdiv, Fdivr][group as usize];
                (
                    mnemonic,
                    if group == 2 || group == 3 {
                        Stack(index)
                    } else {
                        TopStack(index)
                    },
                )
            }
            0xDC | 0xDE if group == 0 || group == 1 || group >= 4 => {
                let mnemonic = if opcode == 0xDC {
                    [Fadd, Fmul, Fcom, Fcomp, Fsubr, Fsub, Fdivr, Fdiv][group as usize]
                } else {
                    [Faddp, Fmulp, Fcom, Fcompp, Fsubrp, Fsubp, Fdivrp, Fdivp][group as usize]
                };
                (mnemonic, StackTop(index))
            }
            0xDE if modrm == 0xD9 => (Fcompp, None),
            0xD9 => match modrm {
                0xC0..=0xC7 => (Fld, Stack(index)),
                0xC8..=0xCF => (Fxch, Stack(index)),
                0xD0 => (Fnop, None),
                0xE0 => (Fchs, None),
                0xE1 => (Fabs, None),
                0xE4 => (Ftst, None),
                0xE5 => (Fxam, None),
                0xE8..=0xEE => (
                    [Fld1, Fldl2t, Fldl2e, Fldpi, Fldlg2, Fldln2, Fldz][index as usize],
                    None,
                ),
                0xF0..=0xFF => (
                    [
                        F2xm1, Fyl2x, Fptan, Fpatan, Fxtract, Fprem1, Fdecstp, Fincstp, Fprem,
                        Fyl2xp1, Fsqrt, Fsincos, Frndint, Fscale, Fsin, Fcos,
                    ][(modrm & 15) as usize],
                    None,
                ),
                _ => return Encoding::Reserved,
            },
            0xDA if modrm == 0xE9 => (Fucompp, None),
            0xDB if modrm >= 0xE0 && modrm <= 0xE4 => (
                [Fneni, Fndisi, Fnclex, Fninit, Fnsetpm][index as usize],
                None,
            ),
            0xDD => match group {
                0 => (Ffree, Stack(index)),
                2 => (Fst, Stack(index)),
                3 => (Fstp, Stack(index)),
                4 => (Fucom, Stack(index)),
                5 => (Fucomp, Stack(index)),
                _ => return Encoding::Reserved,
            },
            0xDF if modrm == 0xE0 => (Fnstsw, Ax),
            0xDA | 0xDB if group <= 3 => {
                later = true;
                let mnemonic = if opcode == 0xDA {
                    [Fcmovb, Fcmove, Fcmovbe, Fcmovu][group as usize]
                } else {
                    [Fcmovnb, Fcmovne, Fcmovnbe, Fcmovnu][group as usize]
                };
                (mnemonic, TopStack(index))
            }
            0xDB | 0xDF if group == 5 || group == 6 => {
                later = true;
                let mnemonic = match (opcode, group) {
                    (0xDB, 5) => Fucomi,
                    (0xDB, 6) => Fcomi,
                    (0xDF, 5) => Fucomip,
                    _ => Fcomip,
                };
                (mnemonic, TopStack(index))
            }
            _ => return Encoding::Reserved,
        }
    };
    let instruction = Instruction { mnemonic, operands };
    if later {
        Encoding::LaterIsa(instruction)
    } else {
        Encoding::I387(instruction)
    }
}
