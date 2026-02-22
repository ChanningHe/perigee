use perigee_core::mac::MacAddress;

use crate::config::MacStrategyConfig;

pub enum MacStrategy {
    Sequential { base_mac: MacAddress },
    Random { seed: Option<u64> },
    Custom(Vec<MacAddress>),
}

impl MacStrategy {
    pub fn from_config(config: &MacStrategyConfig, pf_mac: &MacAddress) -> Self {
        match config {
            MacStrategyConfig::Sequential => Self::Sequential {
                base_mac: *pf_mac,
            },
            MacStrategyConfig::Random => Self::Random { seed: None },
            MacStrategyConfig::Custom => Self::Custom(Vec::new()),
        }
    }

    /// Generate MAC addresses for `count` VFs.
    pub fn generate(&self, count: u32) -> Vec<MacAddress> {
        match self {
            Self::Sequential { base_mac } => (1..=count as u64)
                .map(|offset| base_mac.increment(offset))
                .collect(),
            Self::Random { seed } => match seed {
                Some(s) => (0..count)
                    .map(|i| MacAddress::random_local_seeded(s.wrapping_add(i as u64)))
                    .collect(),
                None => (0..count).map(|_| MacAddress::random_local()).collect(),
            },
            Self::Custom(macs) => macs.clone(),
        }
    }
}

/// Check if any MAC in the list conflicts with known system MACs.
pub fn check_mac_conflicts(
    generated: &[MacAddress],
    system_macs: &[MacAddress],
) -> Vec<(usize, MacAddress)> {
    let mut conflicts = Vec::new();
    for (i, mac) in generated.iter().enumerate() {
        if system_macs.contains(mac) {
            conflicts.push((i, *mac));
        }
        // Also check internal duplicates
        for (j, other) in generated.iter().enumerate() {
            if i != j && mac == other {
                conflicts.push((i, *mac));
                break;
            }
        }
    }
    conflicts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequential_strategy() {
        let base: MacAddress = "b8:ce:f6:12:34:56".parse().unwrap();
        let strategy = MacStrategy::Sequential { base_mac: base };
        let macs = strategy.generate(3);
        assert_eq!(macs[0].to_string(), "b8:ce:f6:12:34:57");
        assert_eq!(macs[1].to_string(), "b8:ce:f6:12:34:58");
        assert_eq!(macs[2].to_string(), "b8:ce:f6:12:34:59");
    }

    #[test]
    fn random_strategy_generates_local_unicast() {
        let strategy = MacStrategy::Random { seed: Some(42) };
        let macs = strategy.generate(5);
        for mac in &macs {
            assert!(mac.is_unicast());
            assert!(mac.is_locally_administered());
        }
    }

    #[test]
    fn conflict_detection() {
        let mac1: MacAddress = "aa:bb:cc:dd:ee:01".parse().unwrap();
        let mac2: MacAddress = "aa:bb:cc:dd:ee:02".parse().unwrap();
        let generated = vec![mac1, mac2];
        let system = vec![mac1];
        let conflicts = check_mac_conflicts(&generated, &system);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].0, 0);
    }
}
