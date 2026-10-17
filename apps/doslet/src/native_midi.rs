//! Native MIDI adapter. System ports synthesize externally; no sample rendering.
use doslet_midi_synth::wire::Decoder;
use midir::{MidiOutput, MidiOutputPort};
use std::time::Instant;
mod scheduler;
pub use scheduler::LOOKAHEAD;
pub struct Ports {
    output: MidiOutput,
    ports: Vec<MidiOutputPort>,
    pub names: Vec<String>,
}
pub fn ports() -> Result<Ports, String> {
    let output = MidiOutput::new("Doslet").map_err(|e| format!("MIDI unavailable: {e}"))?;
    let ports = output.ports();
    let names = ports
        .iter()
        .enumerate()
        .map(|(i, port)| {
            output
                .port_name(port)
                .map(|name| format!("{i}: {name}"))
                .map_err(|e| e.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Ports {
        output,
        ports,
        names,
    })
}
pub struct Output {
    scheduler: scheduler::Output,
    decoder: Decoder,
    pub name: String,
    active: bool,
}
impl Ports {
    pub fn connect(self, selection: &str) -> Result<Output, String> {
        let index = port_index(&self.names, selection)
            .ok_or_else(|| if selection.eq_ignore_ascii_case("timidity") {
                "TiMidity MIDI port unavailable; start its ALSA sequencer service and refresh outputs".into()
            } else {
                format!("Unknown MIDI output {selection:?}; use --list-midi-outputs")
            })?;
        let name = self.names[index].clone();
        let connection = self
            .output
            .connect(&self.ports[index], "Doslet guest MIDI")
            .map_err(|e| e.to_string())?;
        Ok(Output {
            scheduler: scheduler::Output::new(connection)?,
            decoder: Decoder::new(),
            name,
            active: false,
        })
    }
}

fn port_index(names: &[String], selection: &str) -> Option<usize> {
    names
        .iter()
        .position(|name| {
            name == selection
                || name
                    .split_once(": ")
                    .is_some_and(|(_, raw)| raw == selection)
        })
        .or_else(|| selection.parse::<usize>().ok().filter(|i| *i < names.len()))
        .or_else(|| {
            selection
                .eq_ignore_ascii_case("timidity")
                .then(|| {
                    names
                        .iter()
                        .position(|name| name.to_ascii_lowercase().contains("timidity"))
                })
                .flatten()
        })
}

impl Output {
    #[cfg(test)]
    pub(crate) fn mock() -> (Self, std::sync::mpsc::Receiver<(Instant, Vec<u8>)>) {
        let (scheduler, receiver) = scheduler::Output::mock();
        (
            Self {
                scheduler,
                decoder: Decoder::new(),
                name: "Test MIDI".into(),
                active: false,
            },
            receiver,
        )
    }
    pub fn check(&self) -> Result<(), String> {
        self.scheduler.check()
    }
    pub fn push(&mut self, nanos: u64, bytes: &[u8]) -> Result<(), String> {
        self.scheduler.check()?;
        for &byte in bytes {
            if let Some(message) = self
                .decoder
                .push(byte)
                .map_err(|e| format!("MIDI wire error: {e:?}"))?
                && self.active
            {
                self.scheduler.push(nanos, message)?;
            }
        }
        Ok(())
    }
    pub fn set_active(&mut self, active: bool) -> Result<bool, String> {
        self.scheduler.check()?;
        if self.active == active {
            return Ok(false);
        }
        self.active = active;
        self.decoder = Decoder::new();
        self.scheduler.set_active(active);
        Ok(active)
    }
    pub fn start_clock(&self, nanos: u64, now: Instant) {
        self.scheduler.start_clock(nanos, now);
    }
    pub fn clear(&mut self) -> Result<(), String> {
        self.scheduler.clear();
        self.decoder = Decoder::new();
        self.scheduler.check()
    }
    pub fn label(&self) -> String {
        let (sent, queued, late, max_late) = self.scheduler.stats();
        format!(
            "Host MIDI · {} · {sent} sent · {queued} queued · {late} late (>2 ms) · max {max_late:.1} ms",
            self.name
        )
    }
}

#[cfg(test)]
mod selection_tests {
    use super::port_index;
    #[test]
    fn timidity_alias_discovers_running_port_without_changing_explicit_selection() {
        let ports = vec![
            "0: External synth".into(),
            "1: TiMidity port 0".into(),
            "2: TiMidity port 1".into(),
        ];
        assert_eq!(port_index(&ports, "timidity"), Some(1));
        assert_eq!(port_index(&ports, "TiMidity"), Some(1));
        assert_eq!(port_index(&ports, "2"), Some(2));
        assert_eq!(port_index(&ports, "TiMidity port 1"), Some(2));
        assert_eq!(port_index(&ports, "0: External synth"), Some(0));
        assert_eq!(port_index(&ports[..1], "timidity"), None);
    }
}
