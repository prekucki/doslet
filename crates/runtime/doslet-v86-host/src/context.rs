use super::*;
use doslet_v86::{Context, Segment};

impl<B: CpuBus + Send + 'static> Cpu<B> {
    pub fn context(&mut self) -> Result<Context> {
        let started = self.profiling.then(std::time::Instant::now);
        let registers = self.registers()?;
        let fpu_pointer = self
            .instance
            .get_typed_func::<(), u32>(&mut self.store, "doslet_fpu_image")?
            .call(&mut self.store, ())?;
        let bytes = self.memory.data(&self.store);
        let fpu = bytes[fpu_pointer as usize..fpu_pointer as usize + 108]
            .try_into()
            .expect("module-owned FPU image");
        let mut segments = core::array::from_fn(|index| Segment {
            selector: u16::from_le_bytes(
                bytes[668 + index * 2..670 + index * 2].try_into().unwrap(),
            ),
            base: self.word(736 + index * 4),
            limit: self.word(768 + index * 4),
            access: bytes[512 + index],
            default32: match index {
                1 => bytes[804] != 0,
                2 => bytes[808] != 0,
                _ => false,
            },
            usable: bytes[724 + index] == 0,
        });
        for segment in &mut segments[6..] {
            if segment.selector & !3 == 0 {
                *segment = Segment::default();
            }
        }
        let context = Context {
            registers,
            segments,
            cr: core::array::from_fn(|index| self.word(580 + index * 4)),
            gdtr: (self.word(576), self.word(572) as u16),
            idtr: (self.word(568), self.word(564) as u16),
            cpl: bytes[612],
            protected: bytes[800] != 0,
            code32: bytes[804] != 0,
            stack32: bytes[808] != 0,
            halted: bytes[616] != 0,
            interrupt_shadow: self.interrupt_shadow,
            fpu,
        };
        if let Some(started) = started {
            self.store.data_mut().stats.context_nanos += started.elapsed().as_nanos() as u64;
        }
        Ok(context)
    }

    /// Synchronize architectural values after a host DOS/BIOS service. Guest
    /// RAM is not transferred. Ordinary execution keeps all state in v86.
    pub fn restore_context(&mut self, state: &Context) -> Result<()> {
        if self.interrupt_shadow != state.interrupt_shadow {
            self.instance
                .get_typed_func::<u32, ()>(&mut self.store, "doslet_set_interrupt_shadow")?
                .call(&mut self.store, u32::from(state.interrupt_shadow))?;
            self.interrupt_shadow = state.interrupt_shadow;
        }
        for (index, value) in state.registers.gpr.iter().enumerate() {
            self.memory
                .write(&mut self.store, 64 + index * 4, &value.to_le_bytes())?;
        }
        for (index, segment) in state.segments.iter().enumerate() {
            self.memory.write(
                &mut self.store,
                668 + index * 2,
                &segment.selector.to_le_bytes(),
            )?;
            self.memory.write(
                &mut self.store,
                736 + index * 4,
                &segment.base.to_le_bytes(),
            )?;
            self.memory.write(
                &mut self.store,
                768 + index * 4,
                &segment.limit.to_le_bytes(),
            )?;
            self.memory
                .write(&mut self.store, 512 + index, &[segment.access])?;
            self.memory
                .write(&mut self.store, 724 + index, &[u8::from(!segment.usable)])?;
        }
        let ip = state.segments[1].base.wrapping_add(state.registers.eip);
        for (offset, value) in [
            (556, ip),
            (120, state.registers.eflags),
            (100, 0),
            (576, state.gdtr.0),
            (572, u32::from(state.gdtr.1)),
            (568, state.idtr.0),
            (564, u32::from(state.idtr.1)),
            (800, u32::from(state.protected)),
            (804, u32::from(state.code32)),
            (808, u32::from(state.stack32)),
            (616, u32::from(state.halted)),
        ] {
            self.memory
                .write(&mut self.store, offset, &value.to_le_bytes())?;
        }
        self.memory.write(&mut self.store, 612, &[state.cpl])?;
        self.memory.write(
            &mut self.store,
            1128,
            &[u8::from(
                state.segments[doslet_v86::TASK_SEGMENT].access & 8 != 0,
            )],
        )?;
        for (index, value) in state.cr.iter().enumerate() {
            self.memory
                .write(&mut self.store, 580 + index * 4, &value.to_le_bytes())?;
        }
        let pointer = self
            .instance
            .get_typed_func::<(), u32>(&mut self.store, "doslet_fpu_image")?
            .call(&mut self.store, ())?;
        self.memory
            .write(&mut self.store, pointer as usize, &state.fpu)?;
        self.instance
            .get_typed_func::<(), ()>(&mut self.store, "doslet_rebind_context")?
            .call(&mut self.store, ())
    }

    pub fn set_a20(&mut self, enabled: bool) -> Result<()> {
        self.instance
            .get_typed_func::<u32, ()>(&mut self.store, "doslet_set_a20")?
            .call(&mut self.store, u32::from(enabled))
    }

    pub fn set_machine_boundaries(&mut self, enabled: bool) -> Result<()> {
        self.background = enabled;
        self.instance
            .get_typed_func::<u32, ()>(&mut self.store, "doslet_machine_boundaries")?
            .call(&mut self.store, u32::from(enabled))
    }

    pub fn set_wait_boundary(&mut self, physical: Option<u32>) -> Result<()> {
        self.instance
            .get_typed_func::<u32, ()>(&mut self.store, "doslet_wait_boundary")?
            .call(&mut self.store, physical.unwrap_or(u32::MAX))
    }
}
