//! Opt-in bus specialization. Ordinary buses contain no observation callback.
use crate::{BusError, CodeCheck, CpuBus, MemoryBus, PortBus};

pub const EVENTS: usize = 64;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Read,
    Write,
    PortRead,
    PortWrite,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    pub kind: Kind,
    pub address: u32,
    pub width: u8,
    pub value: u32,
    /// Before a RAM write, from a side-effect-free ordinary-memory view only.
    pub previous: Option<u32>,
}
#[derive(Clone, Debug)]
pub struct Trace {
    pub events: [Option<Event>; EVENTS],
    pub len: usize,
    pub incomplete: bool,
}
impl Default for Trace {
    fn default() -> Self {
        Self {
            events: [None; EVENTS],
            len: 0,
            incomplete: false,
        }
    }
}
impl Trace {
    fn record(&mut self, kind: Kind, address: u32, width: u8, value: u32, previous: Option<u32>) {
        if self.len == EVENTS {
            self.incomplete = true;
            return;
        }
        self.events[self.len] = Some(Event {
            kind,
            address,
            width,
            value,
            previous,
        });
        self.len += 1;
    }
}
pub struct Observed<'a, B> {
    pub bus: &'a mut B,
    pub trace: &'a mut Trace,
}
macro_rules! read {
    ($name:ident,$ty:ty,$width:expr,$kind:ident) => {
        fn $name(&mut self, address: u32) -> Result<$ty, BusError> {
            let result = self.bus.$name(address);
            match result {
                Ok(v) => self
                    .trace
                    .record(Kind::$kind, address, $width, u32::from(v), None),
                Err(_) => self.trace.incomplete = true,
            }
            result
        }
    };
}
macro_rules! write {
    ($name:ident,$ty:ty,$width:expr) => {
        fn $name(&mut self, address: u32, value: $ty) -> Result<(), BusError> {
            let previous = if self.bus.is_plain_ram(address, $width) {
                self.bus.code_bytes(address, $width).map(|bytes| {
                    bytes
                        .iter()
                        .enumerate()
                        .fold(0, |v, (i, b)| v | (u32::from(*b) << (i * 8)))
                })
            } else {
                None
            };
            let result = self.bus.$name(address, value);
            // Failed wide writes may already have committed a prefix. Do not
            // manufacture a complete transaction or silently lose that ambiguity.
            if result.is_ok() {
                self.trace
                    .record(Kind::Write, address, $width, u32::from(value), previous);
            } else {
                self.trace.incomplete = true;
            }
            result
        }
    };
}
impl<B: CpuBus> MemoryBus for Observed<'_, B> {
    read!(read_u8, u8, 1, Read);
    read!(read_u16, u16, 2, Read);
    read!(read_u32, u32, 4, Read);
    write!(write_u8, u8, 1);
    write!(write_u16, u16, 2);
    write!(write_u32, u32, 4);
    fn code_check(&self, a: u32, n: u8) -> Option<CodeCheck> {
        self.bus.code_check(a, n)
    }
    fn code_valid(&self, c: &CodeCheck) -> bool {
        self.bus.code_valid(c)
    }
    fn code_bytes(&self, a: u32, n: u8) -> Option<&[u8]> {
        self.bus.code_bytes(a, n)
    }
}
macro_rules! port_read {
    ($name:ident,$ty:ty,$width:expr) => {
        fn $name(&mut self, port: u16) -> Result<$ty, BusError> {
            let result = self.bus.$name(port);
            match result {
                Ok(v) => {
                    self.trace
                        .record(Kind::PortRead, u32::from(port), $width, u32::from(v), None)
                }
                Err(_) => self.trace.incomplete = true,
            }
            result
        }
    };
}
macro_rules! port_write {
    ($name:ident,$ty:ty,$width:expr) => {
        fn $name(&mut self, port: u16, value: $ty) -> Result<(), BusError> {
            let result = self.bus.$name(port, value);
            if result.is_ok() {
                self.trace.record(
                    Kind::PortWrite,
                    u32::from(port),
                    $width,
                    u32::from(value),
                    None,
                );
            } else {
                self.trace.incomplete = true;
            }
            result
        }
    };
}
impl<B: CpuBus> PortBus for Observed<'_, B> {
    port_read!(read_port_u8, u8, 1);
    port_read!(read_port_u16, u16, 2);
    port_read!(read_port_u32, u32, 4);
    port_write!(write_port_u8, u8, 1);
    port_write!(write_port_u16, u16, 2);
    port_write!(write_port_u32, u32, 4);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Bus {
        reads: u32,
        byte: u8,
    }
    impl MemoryBus for Bus {
        fn is_plain_ram(&self, address: u32, length: u8) -> bool {
            address == 0 && length == 1
        }
        fn code_bytes(&self, address: u32, length: u8) -> Option<&[u8]> {
            self.is_plain_ram(address, length)
                .then(|| core::slice::from_ref(&self.byte))
        }
        fn read_u8(&mut self, _: u32) -> Result<u8, BusError> {
            self.reads += 1;
            Ok(self.byte)
        }
        fn write_u8(&mut self, a: u32, v: u8) -> Result<(), BusError> {
            if a == 1 {
                Err(BusError::UnmappedMemory(a))
            } else {
                self.byte = v;
                Ok(())
            }
        }
    }
    impl PortBus for Bus {
        fn read_port_u8(&mut self, _: u16) -> Result<u8, BusError> {
            self.reads += 1;
            Ok(self.reads as u8)
        }
        fn write_port_u8(&mut self, _: u16, _: u8) -> Result<(), BusError> {
            Ok(())
        }
    }
    #[test]
    fn previous_ram_value_uses_a_view_without_an_extra_guest_read() {
        let mut bus = Bus { byte: 42, reads: 0 };
        let mut trace = Trace::default();
        Observed {
            bus: &mut bus,
            trace: &mut trace,
        }
        .write_u8(0, 7)
        .unwrap();
        assert_eq!(bus.reads, 0);
        let event = trace.events[0].unwrap();
        assert_eq!((event.previous, event.value), (Some(42), 7));
        Observed {
            bus: &mut bus,
            trace: &mut trace,
        }
        .write_port_u8(0x70, 15)
        .unwrap();
        assert_eq!(trace.events[1].unwrap().previous, None);
    }
    #[test]
    fn records_returned_port_values_without_rereads_and_marks_partial_failures() {
        let mut bus = Bus::default();
        let mut trace = Trace::default();
        let mut observed = Observed {
            bus: &mut bus,
            trace: &mut trace,
        };
        assert_eq!(observed.read_port_u8(0x3da), Ok(1));
        assert!(observed.write_u16(0, 0x1234).is_err());
        assert_eq!(bus.byte, 0x34);
        assert_eq!(bus.reads, 1);
        assert!(trace.incomplete);
        assert_eq!(
            trace.events[0],
            Some(Event {
                kind: Kind::PortRead,
                address: 0x3da,
                width: 1,
                value: 1,
                previous: None
            })
        );
    }
    #[test]
    fn capacity_exhaustion_preserves_transactions() {
        let mut bus = Bus::default();
        let mut trace = Trace::default();
        for _ in 0..EVENTS + 3 {
            Observed {
                bus: &mut bus,
                trace: &mut trace,
            }
            .read_port_u8(1)
            .unwrap();
        }
        assert_eq!(bus.reads, EVENTS as u32 + 3);
        assert_eq!(trace.len, EVENTS);
        assert!(trace.incomplete);
    }
}
