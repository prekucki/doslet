//! Host-clock pacing for repeated empty DOS keyboard queries in v86.
//!
//! This is a heuristic, unlike the interpreter's full idle proof. It only
//! yields the host worker after the visible frame has stayed unchanged while
//! the guest repeatedly asks whether a key is available. The CPU and guest
//! clock remain untouched; the next worker turn resumes the instruction after
//! the service, and input or an IRQ can wake the wait earlier.

const MIN_POLLS: u32 = 64;
const STABLE_NANOS: u64 = 250_000_000;
const REVISION_SAMPLE: u32 = 32;

#[derive(Default)]
pub(super) struct InputPoll {
    pub(super) enabled: bool,
    pub(super) parked: bool,
    pub(super) parks: u64,
    revision: Option<u64>,
    stable_since: u64,
    polls: u32,
}

impl InputPoll {
    pub(super) fn reset(&mut self) {
        self.parked = false;
        self.revision = None;
        self.polls = 0;
    }

    pub(super) fn sample_revision(&self) -> bool {
        self.revision.is_none() || self.polls.is_multiple_of(REVISION_SAMPLE)
    }

    pub(super) fn observe_empty(&mut self, now: u64, revision: Option<u64>) -> bool {
        if !self.enabled {
            return false;
        }
        if let Some(revision) = revision
            && self.revision != Some(revision)
        {
            self.revision = Some(revision);
            self.stable_since = now;
            self.polls = 0;
        }
        self.polls = self.polls.saturating_add(1);
        if self.polls >= MIN_POLLS && now.saturating_sub(self.stable_since) >= STABLE_NANOS {
            self.parked = true;
            self.parks += 1;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_stable_repeated_empty_polls_park() {
        let mut poll = InputPoll {
            enabled: true,
            ..InputPoll::default()
        };
        for i in 0..MIN_POLLS {
            assert!(
                !poll.observe_empty(if i == 0 { 0 } else { 249_000_000 }, (i == 0).then_some(1))
            );
        }
        assert!(poll.observe_empty(250_000_000, None));
        assert_eq!(poll.parks, 1);
        poll.parked = false;
        assert!(!poll.observe_empty(251_000_000, Some(2)));
        assert!(!poll.parked);
        poll.reset();
        assert_eq!(poll.polls, 0);
    }
}
