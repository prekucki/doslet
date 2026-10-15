//! Presentation of guest MPU traffic, independent of the selected audio output.
#[derive(Default)]
pub struct MidiActivity {
    now: u64,
    bytes: u64,
    notes: u64,
    last_data: Option<u64>,
    last_note: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lamp {
    Idle,
    Data,
    Note,
}

impl MidiActivity {
    pub fn observe(&mut self, nanos: u64, bytes: u64, notes: u64) {
        if nanos < self.now || bytes < self.bytes || notes < self.notes {
            *self = Self::default();
        }
        self.now = nanos;
        if bytes > self.bytes {
            self.last_data = Some(nanos);
        }
        if notes > self.notes {
            self.last_note = Some(nanos);
        }
        self.bytes = bytes;
        self.notes = notes;
    }

    pub fn lamp(&self, running: bool) -> Lamp {
        let recent = |time: Option<u64>| {
            running && time.is_some_and(|t| self.now.saturating_sub(t) < 250_000_000)
        };
        if recent(self.last_note) {
            Lamp::Note
        } else if recent(self.last_data) {
            Lamp::Data
        } else {
            Lamp::Idle
        }
    }

    pub fn description(&self) -> String {
        let last = self.last_note.map_or_else(
            || "No notes received".into(),
            |time| {
                format!(
                    "Last note: {:.1} s ago (game time)",
                    (self.now - time) as f64 / 1e9
                )
            },
        );
        format!(
            "MPU-401 · game output\n{} MIDI bytes · {} Note On events\n{last}\nGreen: new notes · amber: other MIDI data",
            self.bytes, self.notes
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lamp_expires_without_traffic_and_pause_and_reset_do_not_show_stale_notes() {
        let mut activity = MidiActivity::default();
        activity.observe(0, 2, 0);
        assert_eq!(activity.lamp(true), Lamp::Data);
        activity.observe(10_000_000, 3, 1);
        assert_eq!(activity.lamp(true), Lamp::Note);
        assert_eq!(activity.lamp(false), Lamp::Idle);
        activity.observe(300_000_000, 3, 1);
        assert_eq!(activity.lamp(true), Lamp::Idle);
        activity.observe(310_000_000, 6, 1); // Note Off/controller traffic.
        assert_eq!(activity.lamp(true), Lamp::Data);
        activity.observe(0, 0, 0);
        assert_eq!(activity.lamp(true), Lamp::Idle);
        assert!(activity.description().contains("No notes received"));
    }
}
