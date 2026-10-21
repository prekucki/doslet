//! Whole-region Rust-to-WASM diagnostic. FPU import/commit is inside each
//! bounded batch, including a batch of one iteration to expose boundary cost.
//! Guest RAM/source/device admission and production AOT lowering are separate.
use super::*;

impl Kernel {
    pub fn run_locals(&mut self, batch: u32) -> bool {
        if !(1..=64).contains(&batch) {
            return false;
        }
        let mut iteration = 0;
        while iteration < self.frame[0] {
            // The fixture begins and ends each iteration with an empty stack.
            // Only the physical slot immediately below TOP is modified. Do not
            // import unrelated (possibly stale) empty registers.
            let slot = ((self.fpu.status() >> 11).wrapping_sub(1) & 7) as u8;
            let Some(mut region) = self.fpu.pc24_region(0, 1 << slot) else {
                self.frame[3] = 5;
                return false;
            };
            let end = (iteration + batch).min(self.frame[0]);
            while iteration < end {
                let mut value = 0;
                for operation in LOAD..=STORE_INTEGER_POP {
                    let accepted = match operation {
                        LOAD => region.load_real32(BASE + (iteration & 0xffff)),
                        MULTIPLY => region.arithmetic_real32(MULTIPLIER, 1),
                        ADD => region.arithmetic_real32(ADDEND, 0),
                        _ => match region.store_integer32() {
                            Some(result) => {
                                value = result;
                                true
                            }
                            None => false,
                        },
                    };
                    if !accepted {
                        self.frame[3] = operation + 1;
                        region.commit();
                        return false;
                    }
                    region.record_instruction(
                        START_IP + operation * 4,
                        0,
                        [0x106, 0x00e, 0x006, 0x31e][operation as usize],
                    );
                    region.record_operand(OPERAND + operation * 4, 0);
                    if operation == STORE_INTEGER_POP {
                        // No fallible RAM write exists in this diagnostic. A
                        // production caller must prove/write its destination
                        // before popping the stack.
                        assert!(region.pop());
                    }
                }
                self.frame[1] = checksum(self.frame[1], value, iteration);
                iteration += 1;
                self.frame[2] = iteration;
            }
            region.commit();
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_batches_match_reference_retained_state_without_allocations() {
        for rounding in 0..4 {
            let control = 0x7f | (rounding << 10);
            for count in [1, 2, 17, 257] {
                let mut reference = Kernel::default();
                reference.reset(control, count);
                assert!(reference.run(false));
                let expected = reference.state().to_vec();
                for batch in [1, 4, 16, 64] {
                    let mut locals = Kernel::default();
                    locals.reset(control, count);
                    let allocation = allocation_counter::measure(|| {
                        assert!(locals.run_locals(batch));
                    });
                    assert_eq!(allocation.count_total, 0);
                    assert_eq!(locals.state(), expected, "RC={rounding}, batch={batch}");
                }
            }
        }
    }

    #[test]
    fn local_region_refuses_unsupported_control_and_invalid_batches() {
        for control in [0x017f, 0x027f, 0x037f, 0x0c5f, 0x0c7e] {
            let mut locals = Kernel::default();
            locals.reset(control, 1);
            let before = locals.fpu.save().encode_to_vec();
            assert!(!locals.run_locals(16));
            assert_eq!(locals.fpu.save().encode_to_vec(), before);
            assert_eq!(locals.frame(), [1, 0, 0, 5]);
        }
        for batch in [0, 65, u32::MAX] {
            let mut locals = Kernel::default();
            locals.reset(0x0c7f, 1);
            let before = locals.state().to_vec();
            assert!(!locals.run_locals(batch));
            assert_eq!(locals.state(), before);
        }
    }
}
