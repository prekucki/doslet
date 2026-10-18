//! Manual host-side corrections to existing, byte-guarded instruction labels.
use super::*;

impl Controller {
    pub fn code_tags(
        &mut self,
        session: &mut Session,
        start: u32,
        end: u32,
        bits: u8,
        tag: Option<Tier>,
    ) -> Result<serde_json::Value, String> {
        if start >= end || end - start > 65536 || !matches!(bits, 16 | 32) {
            return Err("Expected [start,end) spanning 1..65536 bytes and bits=16 or 32".into());
        }
        if tag.is_some() && !self.stats.enabled {
            return Err("Enable speed tiers before tagging code".into());
        }
        let tier = tag;
        if tag == Some(Tier::Irq) {
            return Err("Confirmed IRQ labels cannot be assigned manually".into());
        }
        let mut counts = [0usize; 4];
        let (mut mismatched, mut protected, mut matched) = (0, 0, 0);
        let mut edits = Vec::new();
        for (&key, node) in self.profile.nodes.range((start, false)..(end, false)) {
            if key.1 != (bits == 32) {
                continue;
            }
            counts[match node.tier {
                Tier::Fast => 0,
                Tier::Slow => 1,
                Tier::Grey => 2,
                Tier::Irq => 3,
            }] += 1;
            let mut code = [0; 15];
            if !session.debug_read_memory(key.0, &mut code) || node.code != code {
                mismatched += 1;
                continue;
            }
            matched += 1;
            if node.locked || node.tier == Tier::Irq {
                protected += 1;
                continue;
            }
            if let Some(tier) = tier
                && (node.tier != tier || !node.manual)
            {
                edits.push((key, tier));
            }
        }
        if !edits.is_empty() {
            // A replay captured before this edit must never install stale evidence.
            if let Some(learner) = &mut self.learner {
                learner.cancel();
            }
            for (key, tier) in &edits {
                let node = self.profile.nodes.get_mut(key).expect("existing label");
                node.tier = *tier;
                node.manual = true;
            }
            let labels = self
                .profile
                .nodes
                .iter()
                .map(|(&(address, code32), n)| doslet_x86::ExecutionLabel {
                    address,
                    code32,
                    bytes: n.code,
                    tier: match n.tier {
                        Tier::Fast => 0,
                        Tier::Slow => 1,
                        Tier::Grey => 2,
                        Tier::Irq => 3,
                    },
                })
                .collect();
            session
                .replace_execution_labels(labels)
                .expect("validated existing labels");
            if let Some(p) = &mut self.persistence {
                p.changed();
            }
            self.maintain_persistence();
        }
        Ok(serde_json::json!({
            "start": start, "end": end, "bits": bits,
            "previous_counts": {"hot": counts[0], "cold": counts[1], "grey": counts[2], "irq": counts[3]},
            "matching": matched, "code_mismatches": mismatched, "protected": protected,
            "changed": edits.len(), "tier": tag.map(|t| match t {
                Tier::Fast => "hot", Tier::Slow => "cold", Tier::Grey => "grey", Tier::Irq => "irq"
            }),
            "save_pending": self.persistence.as_ref().is_some_and(|p| p.pending()),
            "persistent": self.persistence.is_some(),
        }))
    }
}
