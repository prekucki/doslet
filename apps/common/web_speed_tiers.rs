//! Shared speed-tier wire validation for the packager and browser runtime.
use doslet_x86::ExecutionLabel;
#[derive(Clone)]
pub struct TierProfile {
    pub labels: Vec<ExecutionLabel>,
    pub rates: [Option<u16>; 3],
}

pub fn parse_tier_profile(input: &str) -> Result<TierProfile, String> {
    let value: serde_json::Value = serde_json::from_str(input).map_err(|e| e.to_string())?;
    if value["unknown"] != "grey" || value["irq_policy"] != "static-grey" {
        return Err("Speed-tier profile requires unknown=grey and irq_policy=static-grey".into());
    }
    let rates = value["rates"]
        .as_array()
        .ok_or("Missing speed-tier rates")?;
    if rates.len() != 3 {
        return Err("Expected HOT, COLD and GREY rates".into());
    }
    let rate = |value: &serde_json::Value| -> Result<Option<u16>, String> {
        if value.is_null() {
            return Ok(None);
        }
        let number = value.as_u64().ok_or("Invalid speed-tier rate")?;
        if !(1..=1000).contains(&number) {
            return Err("Speed-tier rates must be 1..1000 MHz".into());
        }
        Ok(Some(number as u16))
    };
    let rates = [rate(&rates[0])?, rate(&rates[1])?, rate(&rates[2])?];
    if rates[1].is_none() || rates[2].is_none() {
        return Err("COLD and GREY rates are required".into());
    }
    let nodes = value["nodes"]
        .as_array()
        .ok_or("Missing speed-tier labels")?;
    if nodes.len() > 100_000 {
        return Err("Too many speed-tier labels".into());
    }
    let mut labels = Vec::with_capacity(nodes.len());
    for node in nodes {
        let address = u32::try_from(node["pc"].as_u64().ok_or("Invalid label address")?)
            .map_err(|_| "Invalid label address")?;
        let code32 = match node["bits"].as_u64() {
            Some(16) => false,
            Some(32) => true,
            _ => return Err("Invalid label bitness".into()),
        };
        let bytes = node["bytes"].as_array().ok_or("Missing label bytes")?;
        if bytes.len() != 15 {
            return Err("A label must guard 15 instruction bytes".into());
        }
        let mut guard = [0; 15];
        for (out, byte) in guard.iter_mut().zip(bytes) {
            *out = u8::try_from(byte.as_u64().ok_or("Invalid label byte")?)
                .map_err(|_| "Invalid label byte")?;
        }
        let tier = match node["tier"].as_str() {
            Some("fast") => 0,
            Some("slow") => 1,
            Some("grey") => 2,
            Some("irq") => 3,
            _ => return Err("Invalid speed tier".into()),
        };
        labels.push(ExecutionLabel {
            address,
            code32,
            bytes: guard,
            tier,
        });
    }
    Ok(TierProfile { labels, rates })
}
