//! Host independent, bounded physical input routing. Axes use -32767..=32767.
#![no_std]

pub const MAX_SOURCES: usize = 16;
pub const MAX_BINDINGS: usize = 64;
pub const MAX_EDGES: usize = 256;
/// USB HID keyboard usage, independent of host layout and ASCII.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhysicalKey(pub u16);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputSourceId(pub u8);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputAction {
    Key { key: PhysicalKey, pressed: bool },
    Button { button: u8, pressed: bool },
    Axis { axis: u8, value: i16 },
    ReleaseSource,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputEvent {
    pub session_epoch: u64,
    pub source: InputSourceId,
    pub connection_generation: u64,
    pub sequence: u64,
    pub action: InputAction,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    Key(PhysicalKey),
    Button(u8),
    Axis(u8),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Endpoint {
    Key(PhysicalKey),
    Button(u8),
    Axis(u8),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    pub source: InputSourceId,
    pub control: Control,
    pub target: Endpoint,
    /// Signed full-scale value for digital controls; axis gain in 1/32767 units.
    pub scale: i16,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuestEvent {
    Key {
        key: PhysicalKey,
        pressed: bool,
    },
    Button {
        button: u8,
        pressed: bool,
    },
    Axis {
        axis: u8,
        value: i16,
    },
    /// Reserved control latch. This event cannot be blocked by edge overflow.
    ReleaseAll,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputError {
    Source,
    BindingsFull,
    InvalidBinding,
    Stale,
    Overflow,
}
#[derive(Clone, Copy, Default)]
struct Source {
    generation: u64,
    sequence: u64,
    connected: bool,
    seen: bool,
    registered: bool,
    keys: [u64; 4],
    buttons: u32,
    axes: [i16; 16],
    keyboard: bool,
    joystick: bool,
    suppressed_keys: [u64; 4],
    suppressed_buttons: u32,
    suppressed_axes: [bool; 16],
}
impl Source {
    fn suppress(&mut self) {
        for i in 0..4 {
            self.suppressed_keys[i] |= self.keys[i];
        }
        self.suppressed_buttons |= self.buttons;
        for i in 0..16 {
            self.suppressed_axes[i] |= self.axes[i] != 0;
        }
    }
    fn value(&self, control: Control) -> i16 {
        match control {
            Control::Key(key) => {
                if self.keys[key.0 as usize / 64] & (1 << (key.0 % 64)) != 0 {
                    32767
                } else {
                    0
                }
            }
            Control::Button(button) => {
                if self.buttons & (1 << button) != 0 {
                    32767
                } else {
                    0
                }
            }
            Control::Axis(axis) => self.axes[axis as usize],
        }
    }
    fn update(&mut self, control: Control, value: i16) -> Result<(), InputError> {
        match control {
            Control::Key(key) if key.0 < 256 => {
                let bit = 1u64 << (key.0 % 64);
                let word = &mut self.keys[key.0 as usize / 64];
                if value != 0 {
                    *word |= bit;
                } else {
                    *word &= !bit;
                    self.suppressed_keys[key.0 as usize / 64] &= !bit;
                }
            }
            Control::Button(button) if button < 32 => {
                let bit = 1 << button;
                if value != 0 {
                    self.buttons |= bit;
                } else {
                    self.buttons &= !bit;
                    self.suppressed_buttons &= !bit;
                }
            }
            Control::Axis(axis) if axis < 16 => {
                self.axes[axis as usize] = value;
                if value == 0 {
                    self.suppressed_axes[axis as usize] = false;
                }
            }
            _ => return Err(InputError::InvalidBinding),
        }
        Ok(())
    }
}

pub struct InputRouter {
    epoch: u64,
    sources: [Source; MAX_SOURCES],
    bindings: [Option<Binding>; MAX_BINDINGS],
    count: usize,
    values: [i16; MAX_BINDINGS],
    held: [bool; MAX_BINDINGS],
    suppressed: [bool; MAX_BINDINGS],
    edges: [GuestEvent; MAX_EDGES],
    head: usize,
    len: usize,
    release_latch: bool,
}
impl Default for InputRouter {
    fn default() -> Self {
        Self::new(0)
    }
}
impl InputRouter {
    pub const fn new(epoch: u64) -> Self {
        Self {
            epoch,
            sources: [Source {
                generation: 0,
                sequence: 0,
                connected: false,
                seen: false,
                registered: false,
                keys: [0; 4],
                buttons: 0,
                axes: [0; 16],
                keyboard: false,
                joystick: false,
                suppressed_keys: [0; 4],
                suppressed_buttons: 0,
                suppressed_axes: [false; 16],
            }; MAX_SOURCES],
            bindings: [None; MAX_BINDINGS],
            count: 0,
            values: [0; MAX_BINDINGS],
            held: [false; MAX_BINDINGS],
            suppressed: [false; MAX_BINDINGS],
            edges: [GuestEvent::ReleaseAll; MAX_EDGES],
            head: 0,
            len: 0,
            release_latch: false,
        }
    }
    /// Validate every rule before replacing the active compiled bindings.
    pub fn set_bindings(&mut self, bindings: &[Binding]) -> Result<(), InputError> {
        if bindings.len() > MAX_BINDINGS {
            return Err(InputError::BindingsFull);
        }
        for (i, b) in bindings.iter().enumerate() {
            let valid_control = match b.control {
                Control::Key(k) => k.0 < 256,
                Control::Button(n) => n < 32,
                Control::Axis(n) => n < 16,
            };
            let valid_target = match b.target {
                Endpoint::Key(k) => k.0 < 256,
                Endpoint::Button(n) => n < 2,
                Endpoint::Axis(n) => n < 2,
            };
            if b.source.0 as usize >= MAX_SOURCES
                || !valid_control
                || !valid_target
                || b.scale == i16::MIN
                || bindings[..i].iter().any(|previous| {
                    previous.source == b.source
                        && previous.control == b.control
                        && previous.target == b.target
                })
            {
                return Err(InputError::InvalidBinding);
            }
        }
        self.release_all();
        self.bindings.fill(None);
        for (slot, b) in self.bindings.iter_mut().zip(bindings) {
            *slot = Some(*b);
        }
        self.held.fill(false);
        self.suppressed.fill(false);
        for (i, b) in bindings.iter().enumerate() {
            self.held[i] = self.sources[b.source.0 as usize].value(b.control) != 0;
            self.suppressed[i] = self.held[i];
        }
        self.count = bindings.len();
        Ok(())
    }
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn reset(&mut self, epoch: u64) {
        self.release_all();
        self.epoch = epoch;
        self.sources.fill(Source::default());
        self.held.fill(false);
        self.suppressed.fill(false);
    }
    pub fn connect(&mut self, source: InputSourceId, generation: u64) -> Result<(), InputError> {
        let index = source.0 as usize;
        let old = *self.sources.get(index).ok_or(InputError::Source)?;
        if old.connected && old.generation == generation {
            return Ok(());
        }
        if old.registered && generation <= old.generation {
            return Err(InputError::Stale);
        }
        self.release_source(source)?;
        self.sources[index] = Source {
            generation,
            sequence: 0,
            connected: true,
            seen: false,
            registered: true,
            keys: [0; 4],
            buttons: 0,
            axes: [0; 16],
            keyboard: false,
            joystick: false,
            suppressed_keys: [0; 4],
            suppressed_buttons: 0,
            suppressed_axes: [false; 16],
        };
        Ok(())
    }
    /// Built-in physical passthrough consumes no editable binding slots.
    pub fn connect_keyboard(
        &mut self,
        source: InputSourceId,
        generation: u64,
    ) -> Result<(), InputError> {
        self.connect(source, generation)?;
        self.sources[source.0 as usize].keyboard = true;
        Ok(())
    }
    pub fn connect_joystick(
        &mut self,
        source: InputSourceId,
        generation: u64,
    ) -> Result<(), InputError> {
        self.connect(source, generation)?;
        self.sources[source.0 as usize].joystick = true;
        Ok(())
    }
    pub fn accept_key(&mut self, key: PhysicalKey, pressed: bool) -> Result<(), InputError> {
        self.connect_keyboard(InputSourceId(0), 1)?;
        self.accept_default(0, InputAction::Key { key, pressed })
    }
    pub fn accept_joystick(
        &mut self,
        axes: [i16; 2],
        buttons: [bool; 2],
    ) -> Result<(), InputError> {
        self.connect_joystick(InputSourceId(1), 1)?;
        for (axis, value) in axes.into_iter().enumerate() {
            self.accept_default(
                1,
                InputAction::Axis {
                    axis: axis as u8,
                    value,
                },
            )?;
        }
        for (button, pressed) in buttons.into_iter().enumerate() {
            self.accept_default(
                1,
                InputAction::Button {
                    button: button as u8,
                    pressed,
                },
            )?;
        }
        Ok(())
    }
    fn accept_default(&mut self, source: u8, action: InputAction) -> Result<(), InputError> {
        let sequence = self.sources[source as usize]
            .sequence
            .checked_add(1)
            .ok_or(InputError::Stale)?;
        self.push(InputEvent {
            session_epoch: self.epoch,
            source: InputSourceId(source),
            connection_generation: 1,
            sequence,
            action,
        })
    }
    pub fn disconnect(&mut self, source: InputSourceId) -> Result<(), InputError> {
        self.release_source(source)?;
        self.sources[source.0 as usize].connected = false;
        Ok(())
    }
    /// Pause and capture loss use this same bounded release operation.
    pub fn release_all(&mut self) {
        for source in &mut self.sources {
            source.suppress();
        }
        self.values.fill(0);
        for i in 0..self.count {
            self.suppressed[i] |= self.held[i];
        }
        self.head = 0;
        self.len = 0;
        self.release_latch = true;
    }
    pub fn release_source(&mut self, source: InputSourceId) -> Result<(), InputError> {
        if source.0 as usize >= MAX_SOURCES {
            return Err(InputError::Source);
        }
        let before = core::array::from_fn::<_, 260, _>(|i| self.aggregate(Self::endpoint_at(i)));
        self.sources[source.0 as usize].suppress();
        for i in 0..self.count {
            if self.bindings[i].is_some_and(|b| b.source == source) {
                self.suppressed[i] |= self.held[i];
                self.values[i] = 0;
            }
        }
        for (i, before) in before.into_iter().enumerate() {
            let target = Self::endpoint_at(i);
            self.publish(target, before, self.aggregate(target))?;
        }
        Ok(())
    }
    fn endpoint_at(index: usize) -> Endpoint {
        match index {
            0..=255 => Endpoint::Key(PhysicalKey(index as u16)),
            256..=257 => Endpoint::Button((index - 256) as u8),
            _ => Endpoint::Axis((index - 258) as u8),
        }
    }
    pub fn push(&mut self, event: InputEvent) -> Result<(), InputError> {
        let source = self
            .sources
            .get_mut(event.source.0 as usize)
            .ok_or(InputError::Source)?;
        if event.session_epoch != self.epoch
            || !source.connected
            || source.generation != event.connection_generation
            || (source.seen && event.sequence <= source.sequence)
        {
            return Err(InputError::Stale);
        }
        source.sequence = event.sequence;
        source.seen = true;
        if event.action == InputAction::ReleaseSource {
            return self.release_source(event.source);
        }
        let (control, value) = match event.action {
            InputAction::Key { key, pressed } => {
                (Control::Key(key), if pressed { 32767 } else { 0 })
            }
            InputAction::Button { button, pressed } => {
                (Control::Button(button), if pressed { 32767 } else { 0 })
            }
            InputAction::Axis { axis, value } => (Control::Axis(axis), value.max(-32767)),
            InputAction::ReleaseSource => unreachable!(),
        };
        let routing = &self.sources[event.source.0 as usize];
        let passthrough = match control {
            Control::Key(key) if routing.keyboard && key.0 < 256 => Some(Endpoint::Key(key)),
            Control::Button(button) if routing.joystick && button < 2 => {
                Some(Endpoint::Button(button))
            }
            Control::Axis(axis) if routing.joystick && axis < 2 => Some(Endpoint::Axis(axis)),
            _ => None,
        };
        let before = passthrough.map(|target| self.aggregate(target));
        self.sources[event.source.0 as usize].update(control, value)?;
        if let (Some(target), Some(before)) = (passthrough, before) {
            self.publish(target, before, self.aggregate(target))?;
        }
        for i in 0..self.count {
            let Some(binding) = self.bindings[i] else {
                continue;
            };
            if binding.source != event.source || binding.control != control {
                continue;
            }
            self.held[i] = value != 0;
            if value == 0 {
                self.suppressed[i] = false;
            }
            let scaled = if self.suppressed[i] {
                0
            } else {
                (i32::from(value) * i32::from(binding.scale) / 32767) as i16
            };
            self.update(i, scaled)?;
        }
        Ok(())
    }
    fn aggregate(&self, target: Endpoint) -> i16 {
        let mut sum = 0i32;
        let mut pressed = false;
        for i in 0..self.count {
            if self.bindings[i].is_some_and(|b| b.target == target) {
                sum += i32::from(self.values[i]);
                pressed |= self.values[i] != 0;
            }
        }
        for source in &self.sources {
            if !source.connected {
                continue;
            }
            match target {
                Endpoint::Key(key) if source.keyboard => {
                    pressed |= source.keys[key.0 as usize / 64]
                        & !source.suppressed_keys[key.0 as usize / 64]
                        & (1 << (key.0 % 64))
                        != 0
                }
                Endpoint::Button(button) if source.joystick => {
                    pressed |= source.buttons & !source.suppressed_buttons & (1 << button) != 0
                }
                Endpoint::Axis(axis)
                    if source.joystick && !source.suppressed_axes[axis as usize] =>
                {
                    sum += i32::from(source.axes[axis as usize])
                }
                _ => {}
            }
        }
        match target {
            Endpoint::Axis(_) => sum.clamp(-32767, 32767) as i16,
            _ => i16::from(pressed),
        }
    }
    fn update(&mut self, index: usize, value: i16) -> Result<(), InputError> {
        let target = self.bindings[index].unwrap().target;
        let before = self.aggregate(target);
        self.values[index] = value;
        let after = self.aggregate(target);
        self.publish(target, before, after)
    }
    fn publish(&mut self, target: Endpoint, before: i16, after: i16) -> Result<(), InputError> {
        if before == after {
            return Ok(());
        }
        let event = match target {
            Endpoint::Key(key) => GuestEvent::Key {
                key,
                pressed: after != 0,
            },
            Endpoint::Button(button) => GuestEvent::Button {
                button,
                pressed: after != 0,
            },
            Endpoint::Axis(axis) => GuestEvent::Axis { axis, value: after },
        };
        if let GuestEvent::Axis { axis, .. } = event
            && self.len != 0
        {
            let last = (self.head + self.len - 1) % MAX_EDGES;
            if matches!(self.edges[last],GuestEvent::Axis{axis:previous,..}if previous==axis) {
                self.edges[last] = event;
                return Ok(());
            }
        }
        if self.len == MAX_EDGES {
            self.release_all();
            return Err(InputError::Overflow);
        }
        self.edges[(self.head + self.len) % MAX_EDGES] = event;
        self.len += 1;
        Ok(())
    }
    pub fn next_event(&mut self) -> Option<GuestEvent> {
        if self.release_latch {
            self.release_latch = false;
            return Some(GuestEvent::ReleaseAll);
        }
        if self.len == 0 {
            return None;
        }
        let event = self.edges[self.head];
        self.head = (self.head + 1) % MAX_EDGES;
        self.len -= 1;
        Some(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(source: u8, sequence: u64, pressed: bool) -> InputEvent {
        InputEvent {
            session_epoch: 3,
            source: InputSourceId(source),
            connection_generation: 1,
            sequence,
            action: InputAction::Key {
                key: PhysicalKey(4),
                pressed,
            },
        }
    }
    #[test]
    fn contributors_and_reserved_overflow_release() {
        let mut r = InputRouter::new(3);
        r.set_bindings(&[0, 1].map(|source| Binding {
            source: InputSourceId(source),
            control: Control::Key(PhysicalKey(4)),
            target: Endpoint::Key(PhysicalKey(4)),
            scale: 32767,
        }))
        .unwrap();
        r.next_event();
        for s in 0..2 {
            r.connect(InputSourceId(s), 1).unwrap();
            r.push(event(s, 0, true)).unwrap();
        }
        assert_eq!(
            r.next_event(),
            Some(GuestEvent::Key {
                key: PhysicalKey(4),
                pressed: true
            })
        );
        r.push(event(0, 1, false)).unwrap();
        assert_eq!(r.next_event(), None);
        r.push(event(1, 1, false)).unwrap();
        assert_eq!(
            r.next_event(),
            Some(GuestEvent::Key {
                key: PhysicalKey(4),
                pressed: false
            })
        );
        for sequence in 2..258 {
            r.push(event(0, sequence, sequence % 2 == 0)).unwrap();
        }
        assert_eq!(r.push(event(0, 258, true)), Err(InputError::Overflow));
        assert_eq!(r.next_event(), Some(GuestEvent::ReleaseAll));
        assert_eq!(r.next_event(), None);
        r.push(event(0, 259, true)).unwrap();
        assert_eq!(r.next_event(), None);
        r.push(event(0, 260, false)).unwrap();
        r.push(event(0, 261, true)).unwrap();
        assert!(r.next_event().is_some());
    }
    #[test]
    fn stale_connections_and_epochs_are_rejected() {
        let mut r = InputRouter::new(3);
        r.connect(InputSourceId(0), 1).unwrap();
        r.push(event(0, 1, true)).unwrap();
        assert_eq!(r.push(event(0, 1, false)), Err(InputError::Stale));
        r.disconnect(InputSourceId(0)).unwrap();
        assert_eq!(r.push(event(0, 2, false)), Err(InputError::Stale));
        r.connect(InputSourceId(0), 2).unwrap();
        assert_eq!(r.push(event(0, 3, false)), Err(InputError::Stale));
    }
    #[test]
    fn builtin_keyboard_routes_all_positions_without_binding_slots_and_tracks_contributors() {
        let mut r = InputRouter::new(3);
        r.accept_key(PhysicalKey(4), true).unwrap();
        r.connect_keyboard(InputSourceId(2), 1).unwrap();
        r.push(event(2, 1, true)).unwrap();
        assert_eq!(
            r.next_event(),
            Some(GuestEvent::Key {
                key: PhysicalKey(4),
                pressed: true
            })
        );
        assert_eq!(r.next_event(), None);
        r.accept_key(PhysicalKey(4), false).unwrap();
        assert_eq!(r.next_event(), None);
        r.release_source(InputSourceId(2)).unwrap();
        assert_eq!(
            r.next_event(),
            Some(GuestEvent::Key {
                key: PhysicalKey(4),
                pressed: false
            })
        );
        r.accept_key(PhysicalKey(0xE5), true).unwrap();
        assert_eq!(
            r.next_event(),
            Some(GuestEvent::Key {
                key: PhysicalKey(0xE5),
                pressed: true
            })
        );
        r.release_all();
        assert_eq!(r.next_event(), Some(GuestEvent::ReleaseAll));
        r.accept_key(PhysicalKey(0xE5), true).unwrap();
        assert_eq!(r.next_event(), None);
        r.accept_key(PhysicalKey(0xE5), false).unwrap();
        r.accept_key(PhysicalKey(0xE5), true).unwrap();
        assert!(r.next_event().is_some());
    }
    #[test]
    fn builtin_joystick_release_retains_physical_state_until_actual_neutral() {
        let mut r = InputRouter::new(3);
        r.accept_joystick([16000, 0], [true, false]).unwrap();
        assert_eq!(
            r.next_event(),
            Some(GuestEvent::Axis {
                axis: 0,
                value: 16000
            })
        );
        assert_eq!(
            r.next_event(),
            Some(GuestEvent::Button {
                button: 0,
                pressed: true
            })
        );
        r.release_source(InputSourceId(1)).unwrap();
        assert_eq!(
            r.next_event(),
            Some(GuestEvent::Button {
                button: 0,
                pressed: false
            })
        );
        assert_eq!(r.next_event(), Some(GuestEvent::Axis { axis: 0, value: 0 }));
        r.accept_joystick([16000, 0], [true, false]).unwrap();
        assert_eq!(r.next_event(), None);
        r.accept_joystick([0, 0], [false, false]).unwrap();
        r.accept_joystick([-16000, 0], [true, false]).unwrap();
        assert_eq!(
            r.next_event(),
            Some(GuestEvent::Axis {
                axis: 0,
                value: -16000
            })
        );
        assert_eq!(
            r.next_event(),
            Some(GuestEvent::Button {
                button: 0,
                pressed: true
            })
        );
    }
    #[test]
    fn rebind_of_an_unbound_held_key_requires_release_and_connections_cannot_reuse_generations() {
        let mut r = InputRouter::new(3);
        r.connect(InputSourceId(0), 1).unwrap();
        r.push(event(0, 0, true)).unwrap();
        r.set_bindings(&[Binding {
            source: InputSourceId(0),
            control: Control::Key(PhysicalKey(4)),
            target: Endpoint::Key(PhysicalKey(5)),
            scale: 32767,
        }])
        .unwrap();
        assert_eq!(r.next_event(), Some(GuestEvent::ReleaseAll));
        r.push(event(0, 1, true)).unwrap();
        assert_eq!(r.next_event(), None);
        r.push(event(0, 2, false)).unwrap();
        r.push(event(0, 3, true)).unwrap();
        assert_eq!(
            r.next_event(),
            Some(GuestEvent::Key {
                key: PhysicalKey(5),
                pressed: true
            })
        );
        r.connect(InputSourceId(1), 1).unwrap();
        r.disconnect(InputSourceId(1)).unwrap();
        assert_eq!(r.connect(InputSourceId(1), 1), Err(InputError::Stale));
    }
    #[test]
    fn combined_axes_clamp_and_focus_loss_suppresses_until_neutral() {
        let mut r = InputRouter::new(3);
        r.set_bindings(&[0, 1].map(|source| Binding {
            source: InputSourceId(source),
            control: Control::Axis(0),
            target: Endpoint::Axis(0),
            scale: 32767,
        }))
        .unwrap();
        r.next_event();
        for source in 0..2 {
            r.connect(InputSourceId(source), 1).unwrap();
            r.push(InputEvent {
                action: InputAction::Axis {
                    axis: 0,
                    value: 30000,
                },
                ..event(source, 0, false)
            })
            .unwrap();
        }
        assert_eq!(
            r.next_event(),
            Some(GuestEvent::Axis {
                axis: 0,
                value: 32767
            })
        );
        assert_eq!(r.next_event(), None);
        r.release_all();
        assert_eq!(r.next_event(), Some(GuestEvent::ReleaseAll));
        r.push(InputEvent {
            action: InputAction::Axis {
                axis: 0,
                value: 30000,
            },
            ..event(0, 1, false)
        })
        .unwrap();
        assert_eq!(r.next_event(), None);
        r.push(InputEvent {
            action: InputAction::Axis { axis: 0, value: 0 },
            ..event(0, 2, false)
        })
        .unwrap();
        r.push(InputEvent {
            action: InputAction::Axis {
                axis: 0,
                value: -32768,
            },
            ..event(0, 3, false)
        })
        .unwrap();
        assert_eq!(
            r.next_event(),
            Some(GuestEvent::Axis {
                axis: 0,
                value: -32767
            })
        );
    }
}

#[cfg(feature = "save-state")]
#[path = "lib_saved.rs"]
mod saved_state;
