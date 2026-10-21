//! Optional decode-time execution labels. No host clock or machine dependencies.
use alloc::vec::Vec;

/// HOT, COLD, GREY, confirmed IRQ; an absent or changed instruction is GREY.
#[derive(Clone, Debug)]
pub struct ExecutionLabel {
    pub address: u32,
    pub code32: bool,
    pub bytes: [u8; 15],
    pub tier: u8,
}

pub(crate) struct ExecutionProfile {
    labels: Vec<ExecutionLabel>,
}
impl ExecutionProfile {
    pub(crate) fn new(mut labels: Vec<ExecutionLabel>) -> Result<Self, &'static str> {
        if labels.len() > 100_000 || labels.iter().any(|l| l.tier > 3) {
            return Err("invalid execution labels");
        }
        labels.sort_unstable_by_key(|l| (l.address, l.code32));
        if labels
            .windows(2)
            .any(|w| (w[0].address, w[0].code32) == (w[1].address, w[1].code32))
        {
            return Err("duplicate execution label");
        }
        Ok(Self { labels })
    }
    pub(crate) fn classify(&self, address: u32, wide: bool, bytes: &[u8]) -> u8 {
        let Ok(i) = self
            .labels
            .binary_search_by_key(&(address, wide), |l| (l.address, l.code32))
        else {
            return 2;
        };
        let l = &self.labels[i];
        if !bytes.is_empty()
            && bytes.len() <= 15
            && (bytes == &l.bytes[..bytes.len()]
                || (wide && crate::translation::same_template32(&l.bytes, bytes)))
        {
            l.tier
        } else {
            2
        }
    }
}
