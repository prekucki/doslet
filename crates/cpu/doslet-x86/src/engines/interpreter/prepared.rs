//! Compact batch operands and decode-time selection of monomorphized handlers.
//! Bus types stay static: a dense code indexes an associated constant fn table.
use super::*;
use core::marker::PhantomData;

const FORMS: usize = 5;
const VARIANTS: usize = 6; // three widths, two effective-address sizes
const INC: usize = 10 * FORMS;
const LEA: usize = INC + 4;
const PUSH: usize = LEA + 1;
const POP: usize = PUSH + 1;
const CALL: usize = POP + 1;
const RET: usize = CALL + 2;
const BRANCH: usize = RET + 1;
const SHIFT: usize = BRANCH + 2 * 17;
const EXTEND: usize = SHIFT + 8 * 4;
const X87: usize = EXTEND + 8;
const GROUPS: usize = X87 + x87_batch::KINDS;

/// One memory expression at most; register-only forms leave its fields unused.
#[derive(Clone, Copy)]
pub(in crate::engines::interpreter) struct Prepared {
    immediate: u32,
    displacement: u32,
    handler: u16,
    length: u8, // high bit remembers native_candidate, low bits are byte length
    destination: u8,
    source: u8,
    base: u8,
    index_scale: u8,
    segment: u8,
}
const _: () = assert!(core::mem::size_of::<Prepared>() == 16);

impl Prepared {
    pub(super) const EMPTY: Self = Self {
        immediate: 0,
        displacement: 0,
        handler: 0,
        length: 0,
        destination: 0,
        source: 0,
        base: NO_REGISTER,
        index_scale: NO_REGISTER,
        segment: 0,
    };

    pub(in crate::engines::interpreter) fn length(self) -> u8 {
        self.length & 15
    }

    #[cfg(all(
        feature = "dynamic-core",
        not(target_arch = "wasm32"),
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    pub(in crate::engines::interpreter) fn native_candidate(self) -> bool {
        self.length & 128 != 0
    }

    pub(super) fn set_tier(&mut self, tier: u8) {
        self.length = (self.length & !0x30) | (tier << 4);
    }
    pub(in crate::engines) fn tier(self) -> usize {
        usize::from((self.length >> 4) & 3)
    }

    pub(super) fn is_fallback(self) -> bool {
        self.handler == 0
    }

    pub(super) fn set_immediate(&mut self, immediate: u32) {
        self.immediate = immediate;
    }

    pub(super) fn set_displacement(&mut self, displacement: u32) {
        self.displacement = displacement;
    }

    pub(super) fn new(i: Instruction) -> Self {
        let mut p = Self {
            length: i.length,
            ..Self::EMPTY
        };
        #[cfg(all(
            feature = "dynamic-core",
            not(target_arch = "wasm32"),
            any(target_arch = "x86_64", target_arch = "aarch64")
        ))]
        if i.native_candidate() {
            p.length |= 128;
        }
        let mut address32 = false;
        let mut memory = |target| {
            if let Target::Memory {
                segment,
                base,
                index,
                scale,
                displacement,
                address32: wide,
            } = target
            {
                p.segment = segment as u8;
                p.base = base;
                p.index_scale = index | (scale << 4);
                p.displacement = displacement;
                address32 = wide;
            }
        };
        let group = match i.operation {
            Operation::Move { target, source } | Operation::Alu { target, source, .. } => {
                let form = match (target, source) {
                    (Target::Register(dst), Value::Operand(Target::Register(src))) => {
                        p.destination = dst;
                        p.source = src;
                        0
                    }
                    (Target::Register(dst), Value::Immediate(value)) => {
                        p.destination = dst;
                        p.immediate = value;
                        1
                    }
                    (Target::Register(dst), Value::Operand(src @ Target::Memory { .. })) => {
                        p.destination = dst;
                        memory(src);
                        2
                    }
                    (dst @ Target::Memory { .. }, Value::Operand(Target::Register(src))) => {
                        p.source = src;
                        memory(dst);
                        3
                    }
                    (dst @ Target::Memory { .. }, Value::Immediate(value)) => {
                        p.immediate = value;
                        memory(dst);
                        4
                    }
                    _ => return p,
                };
                let operation = match i.operation {
                    Operation::Move { .. } => 0,
                    Operation::Alu {
                        operation: 4,
                        store: false,
                        ..
                    } => 9, // TEST
                    Operation::Alu { operation, .. } => usize::from(operation) + 1,
                    _ => unreachable!(),
                };
                operation * FORMS + form
            }
            Operation::Inc {
                register,
                decrement,
            } => {
                p.destination = register;
                INC + usize::from(decrement) * 2
            }
            Operation::IncTarget { target, decrement } => {
                let mem = matches!(target, Target::Memory { .. });
                if let Target::Register(reg) = target {
                    p.destination = reg;
                }
                memory(target);
                INC + usize::from(decrement) * 2 + usize::from(mem)
            }
            Operation::Lea { register, target } => {
                p.destination = register;
                memory(target);
                LEA
            }
            Operation::Push(Value::Operand(Target::Register(reg))) => {
                p.source = reg;
                PUSH
            }
            Operation::Pop(reg) => {
                p.destination = reg;
                POP
            }
            Operation::Call(disp) => {
                p.immediate = disp as u32;
                CALL + usize::from(i.branch32)
            }
            Operation::Return(release) => {
                p.immediate = u32::from(release);
                RET
            }
            Operation::Branch {
                condition,
                displacement,
            } => {
                p.immediate = displacement as u32;
                BRANCH + usize::from(i.branch32) * 17 + condition.map_or(16, usize::from)
            }
            Operation::Shift {
                target,
                operation,
                count,
            } => {
                let mem = matches!(target, Target::Memory { .. });
                if let Target::Register(reg) = target {
                    p.destination = reg;
                }
                memory(target);
                p.immediate = u32::from(count.unwrap_or(0));
                SHIFT
                    + usize::from(operation) * 4
                    + usize::from(mem) * 2
                    + usize::from(count.is_none())
            }
            Operation::MoveExtend {
                register,
                source,
                word,
                signed,
            } => {
                let mem = matches!(source, Target::Memory { .. });
                p.destination = register;
                if let Target::Register(reg) = source {
                    p.source = reg;
                }
                memory(source);
                EXTEND + usize::from(word) * 4 + usize::from(signed) * 2 + usize::from(mem)
            }
            Operation::X87 { x87, target } => {
                p.immediate = x87.pack();
                memory(target);
                X87 + usize::from(x87.kind())
            }
            _ => return p,
        };
        let width = match i.width {
            1 => 0,
            2 => 1,
            4 => 2,
            _ => unreachable!(),
        };
        p.handler = (1 + group * VARIANTS + width * 2 + usize::from(address32)) as u16;
        p
    }

    #[inline(always)]
    fn offset<const A: bool>(self, r: &Registers) -> u32 {
        let value = |code| {
            if code == NO_REGISTER {
                0
            } else {
                let value = r.get32(Gpr32::decode(code));
                if A { value } else { u32::from(value as u16) }
            }
        };
        let offset = value(self.base)
            .wrapping_add(value(self.index_scale & 15) << (self.index_scale >> 4))
            .wrapping_add(self.displacement);
        if A { offset } else { u32::from(offset as u16) }
    }

    #[inline(always)]
    fn address<B: CpuBus, const W: u8, const A: bool>(
        self,
        cpu: &Cpu,
        bus: &B,
        windows: &mut Windows,
        access: Access,
    ) -> Option<u32> {
        windows.linear(
            &cpu.registers,
            bus,
            SegmentRegister::ALL[usize::from(self.segment)],
            self.offset::<A>(&cpu.registers),
            W,
            access,
        )
    }

    #[inline(always)]
    fn finish(self, cpu: &mut Cpu) -> bool {
        cpu.registers.ip = cpu.registers.ip.wrapping_add(u32::from(self.length()));
        true
    }

    #[inline(always)]
    pub(super) fn execute<B: CpuBus>(
        self,
        cpu: &mut Cpu,
        bus: &mut B,
        windows: &mut Windows,
        code: SegmentWindow,
    ) -> bool {
        (Handlers::<B>::TABLE[usize::from(self.handler)])(cpu, bus, windows, code, self)
    }
}

type Handler<B> = fn(&mut Cpu, &mut B, &mut Windows, SegmentWindow, Prepared) -> bool;
struct Handlers<B>(PhantomData<B>);
impl<B: CpuBus> Handlers<B> {
    // Macro expansion creates each function once per form/width/address size.
    // The table is compiler-owned read-only data, never allocated by a CPU.
    const TABLE: [Handler<B>; 1 + GROUPS * VARIANTS] = {
        let mut table = [decline::<B> as Handler<B>; 1 + GROUPS * VARIANTS];
        macro_rules! group {
            ($group:expr, $handler:ident $(, $arg:expr)*) => {{
                let at = 1 + ($group) * VARIANTS;
                table[at] = $handler::<B, 1, false $(, $arg)*>;
                table[at + 1] = $handler::<B, 1, true $(, $arg)*>;
                table[at + 2] = $handler::<B, 2, false $(, $arg)*>;
                table[at + 3] = $handler::<B, 2, true $(, $arg)*>;
                table[at + 4] = $handler::<B, 4, false $(, $arg)*>;
                table[at + 5] = $handler::<B, 4, true $(, $arg)*>;
            }};
        }
        macro_rules! forms {
            ($op:expr) => {
                group!($op * FORMS, binary, $op, 0);
                group!($op * FORMS + 1, binary, $op, 1);
                group!($op * FORMS + 2, binary, $op, 2);
                group!($op * FORMS + 3, binary, $op, 3);
                group!($op * FORMS + 4, binary, $op, 4);
            };
        }
        forms!(0);
        forms!(1);
        forms!(2);
        forms!(3);
        forms!(4);
        forms!(5);
        forms!(6);
        forms!(7);
        forms!(8);
        forms!(9);
        group!(INC, increment, false, false);
        group!(INC + 1, increment, false, true);
        group!(INC + 2, increment, true, false);
        group!(INC + 3, increment, true, true);
        group!(LEA, lea);
        group!(PUSH, push);
        group!(POP, pop);
        group!(CALL, call, false);
        group!(CALL + 1, call, true);
        group!(RET, ret);
        macro_rules! branches {
            ($($cc:expr),*) => { $(
                group!(BRANCH + $cc, branch, false, $cc);
                group!(BRANCH + 17 + $cc, branch, true, $cc);
            )* };
        }
        branches!(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16);
        macro_rules! shifts {
            ($($op:expr),*) => { $(
                group!(SHIFT + $op * 4, shift, $op, false, false);
                group!(SHIFT + $op * 4 + 1, shift, $op, false, true);
                group!(SHIFT + $op * 4 + 2, shift, $op, true, false);
                group!(SHIFT + $op * 4 + 3, shift, $op, true, true);
            )* };
        }
        shifts!(0, 1, 2, 3, 4, 5, 6, 7);
        macro_rules! extend {
            ($word:expr, $signed:expr) => {
                group!(
                    EXTEND + ($word as usize) * 4 + ($signed as usize) * 2,
                    move_extend,
                    $word,
                    $signed,
                    false
                );
                group!(
                    EXTEND + ($word as usize) * 4 + ($signed as usize) * 2 + 1,
                    move_extend,
                    $word,
                    $signed,
                    true
                );
            };
        }
        extend!(false, false);
        extend!(false, true);
        extend!(true, false);
        extend!(true, true);
        // Only the byte-width entries are selected; register forms ignore
        // the address size and share one instance.
        macro_rules! x87 {
            ($($kind:expr),*) => { $(
                let at = 1 + (X87 + $kind) * VARIANTS;
                table[at] = x87::<B, false, $kind>;
                table[at + 1] = if $kind < x87_batch::MEMORY_KINDS {
                    x87::<B, true, $kind>
                } else {
                    x87::<B, false, $kind>
                };
            )* };
        }
        x87!(
            0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23,
            24, 25, 26, 27, 28, 29, 30, 31, 32, 33
        );
        table
    };
}
fn decline<B: CpuBus>(
    _: &mut Cpu,
    _: &mut B,
    _: &mut Windows,
    _: SegmentWindow,
    _: Prepared,
) -> bool {
    false
}

fn binary<B: CpuBus, const W: u8, const A: bool, const OP: usize, const F: usize>(
    cpu: &mut Cpu,
    bus: &mut B,
    windows: &mut Windows,
    _: SegmentWindow,
    p: Prepared,
) -> bool {
    // MOV has no destination read; CMP and TEST have no destination write.
    let store = OP != 8 && OP != 9;
    let address = if F >= 2 {
        let access = if F >= 3 && OP == 0 {
            Access::Write
        } else {
            Access::Read
        };
        let Some(address) = p.address::<B, W, A>(cpu, bus, windows, access) else {
            return false;
        };
        if F >= 3
            && OP != 0
            && store
            && p.address::<B, W, A>(cpu, bus, windows, Access::Write)
                .is_none()
        {
            return false;
        }
        address
    } else {
        0
    };
    let destination = if F >= 3 {
        Place::Ram(address)
    } else {
        Place::Register(p.destination)
    };
    let right = if F == 1 || F == 4 {
        p.immediate
    } else if F == 2 {
        cpu.load(bus, Place::Ram(address), W)
    } else {
        cpu.load(bus, Place::Register(p.source), W)
    };
    let value = if OP == 0 {
        right
    } else {
        let left = cpu.load(bus, destination, W);
        let operation = if OP == 9 { 4 } else { (OP - 1) as u8 };
        let (value, flags) = alu_width(cpu.flags, left, right, operation, W);
        cpu.flags = flags;
        value
    };
    if store {
        cpu.store(bus, destination, W, value);
    }
    p.finish(cpu)
}

fn increment<B: CpuBus, const W: u8, const A: bool, const DEC: bool, const MEM: bool>(
    cpu: &mut Cpu,
    bus: &mut B,
    windows: &mut Windows,
    _: SegmentWindow,
    p: Prepared,
) -> bool {
    let place = if MEM {
        let Some(address) = p.address::<B, W, A>(cpu, bus, windows, Access::Read) else {
            return false;
        };
        if p.address::<B, W, A>(cpu, bus, windows, Access::Write)
            .is_none()
        {
            return false;
        }
        Place::Ram(address)
    } else {
        Place::Register(p.destination)
    };
    let value = cpu.load(bus, place, W);
    let (value, updated) = increment_result(cpu.flags, value, DEC, W);
    cpu.flags = updated;
    cpu.store(bus, place, W, value);
    p.finish(cpu)
}
fn lea<B: CpuBus, const W: u8, const A: bool>(
    cpu: &mut Cpu,
    bus: &mut B,
    _: &mut Windows,
    _: SegmentWindow,
    p: Prepared,
) -> bool {
    let offset = p.offset::<A>(&cpu.registers);
    cpu.store(bus, Place::Register(p.destination), W, offset);
    p.finish(cpu)
}

#[inline(always)]
fn push_address<B: CpuBus, const W: u8>(
    cpu: &Cpu,
    bus: &B,
    windows: &mut Windows,
) -> Option<(u32, u32)> {
    let sp = cpu.stack_pointer().wrapping_sub(u32::from(W));
    let sp = if cpu.registers.stack32() {
        sp
    } else {
        u32::from(sp as u16)
    };
    windows
        .linear(
            &cpu.registers,
            bus,
            SegmentRegister::Ss,
            sp,
            W,
            Access::Write,
        )
        .map(|address| (sp, address))
}
fn push<B: CpuBus, const W: u8, const A: bool>(
    cpu: &mut Cpu,
    bus: &mut B,
    windows: &mut Windows,
    _: SegmentWindow,
    p: Prepared,
) -> bool {
    let Some((sp, address)) = push_address::<B, W>(cpu, bus, windows) else {
        return false;
    };
    let value = cpu.load(bus, Place::Register(p.source), W);
    cpu.store(bus, Place::Ram(address), W, value);
    cpu.set_stack_pointer(sp);
    p.finish(cpu)
}
fn pop<B: CpuBus, const W: u8, const A: bool>(
    cpu: &mut Cpu,
    bus: &mut B,
    windows: &mut Windows,
    _: SegmentWindow,
    p: Prepared,
) -> bool {
    let Some(address) = windows.linear(
        &cpu.registers,
        bus,
        SegmentRegister::Ss,
        cpu.stack_pointer(),
        W,
        Access::Read,
    ) else {
        return false;
    };
    let value = cpu.load(bus, Place::Ram(address), W);
    cpu.adjust_stack(u32::from(W));
    cpu.store(bus, Place::Register(p.destination), W, value);
    p.finish(cpu)
}
#[inline(always)]
fn jump<const WIDE: bool>(cpu: &Cpu, code: SegmentWindow, p: Prepared) -> Option<u32> {
    let target = cpu
        .registers
        .ip
        .wrapping_add(u32::from(p.length()))
        .wrapping_add(p.immediate);
    let target = if WIDE {
        target
    } else {
        u32::from(target as u16)
    };
    code.address(target, 1).map(|_| target)
}
fn call<B: CpuBus, const W: u8, const A: bool, const WIDE: bool>(
    cpu: &mut Cpu,
    bus: &mut B,
    windows: &mut Windows,
    code: SegmentWindow,
    p: Prepared,
) -> bool {
    let Some(target) = jump::<WIDE>(cpu, code, p) else {
        return false;
    };
    let Some((sp, address)) = push_address::<B, W>(cpu, bus, windows) else {
        return false;
    };
    let next = cpu.registers.ip.wrapping_add(u32::from(p.length()));
    cpu.store(bus, Place::Ram(address), W, next);
    cpu.set_stack_pointer(sp);
    cpu.registers.ip = target;
    true
}
fn ret<B: CpuBus, const W: u8, const A: bool>(
    cpu: &mut Cpu,
    bus: &mut B,
    windows: &mut Windows,
    code: SegmentWindow,
    p: Prepared,
) -> bool {
    let Some(address) = windows.linear(
        &cpu.registers,
        bus,
        SegmentRegister::Ss,
        cpu.stack_pointer(),
        W,
        Access::Read,
    ) else {
        return false;
    };
    let target = cpu.load(bus, Place::Ram(address), W);
    if code.address(target, 1).is_none() {
        return false;
    }
    cpu.adjust_stack(u32::from(W));
    cpu.registers.ip = target;
    cpu.adjust_stack(p.immediate);
    true
}
fn branch<B: CpuBus, const W: u8, const A: bool, const WIDE: bool, const CC: u8>(
    cpu: &mut Cpu,
    _: &mut B,
    _: &mut Windows,
    code: SegmentWindow,
    p: Prepared,
) -> bool {
    if CC == 16 || flags::condition(cpu.flags, CC) {
        let Some(target) = jump::<WIDE>(cpu, code, p) else {
            return false;
        };
        cpu.registers.ip = target;
        true
    } else {
        p.finish(cpu)
    }
}
fn shift<B: CpuBus, const W: u8, const A: bool, const OP: u8, const MEM: bool, const CL: bool>(
    cpu: &mut Cpu,
    bus: &mut B,
    windows: &mut Windows,
    _: SegmentWindow,
    p: Prepared,
) -> bool {
    let count = (if CL {
        cpu.registers.get_byte(1)
    } else {
        p.immediate as u8
    }) & 31;
    if count == 0 {
        return p.finish(cpu);
    }
    let place = if MEM {
        let Some(address) = p.address::<B, W, A>(cpu, bus, windows, Access::Read) else {
            return false;
        };
        if p.address::<B, W, A>(cpu, bus, windows, Access::Write)
            .is_none()
        {
            return false;
        }
        Place::Ram(address)
    } else {
        Place::Register(p.destination)
    };
    let original = cpu.load(bus, place, W);
    let (value, flags) = shift_result(cpu.flags, original, OP, count, W);
    cpu.flags = flags;
    cpu.store(bus, place, W, value);
    p.finish(cpu)
}
fn move_extend<
    B: CpuBus,
    const W: u8,
    const A: bool,
    const WORD: bool,
    const SIGNED: bool,
    const MEM: bool,
>(
    cpu: &mut Cpu,
    bus: &mut B,
    windows: &mut Windows,
    _: SegmentWindow,
    p: Prepared,
) -> bool {
    let place = if MEM {
        let address = if WORD {
            p.address::<B, 2, A>(cpu, bus, windows, Access::Read)
        } else {
            p.address::<B, 1, A>(cpu, bus, windows, Access::Read)
        };
        let Some(address) = address else {
            return false;
        };
        Place::Ram(address)
    } else {
        Place::Register(p.source)
    };
    let value = cpu.load(bus, place, if WORD { 2 } else { 1 });
    let value = extend_result(value, WORD, SIGNED);
    cpu.store(bus, Place::Register(p.destination), W, value);
    p.finish(cpu)
}

fn x87<B: CpuBus, const A: bool, const KIND: u8>(
    cpu: &mut Cpu,
    bus: &mut B,
    windows: &mut Windows,
    _: SegmentWindow,
    p: Prepared,
) -> bool {
    let next = cpu.registers.ip.wrapping_add(u32::from(p.length()));
    cpu.x87_execute(bus, windows, X87::unpack::<KIND>(p.immediate), next, |r| {
        Some((
            SegmentRegister::ALL[usize::from(p.segment)],
            p.offset::<A>(r),
        ))
    })
}
