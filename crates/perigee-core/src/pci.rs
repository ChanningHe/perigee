use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PciAddress {
    pub domain: u16,
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

impl PciAddress {
    pub fn parse(s: &str) -> Option<Self> {
        // Format: "0000:41:00.0"
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() != 3 {
            return None;
        }
        let domain = u16::from_str_radix(parts[0], 16).ok()?;
        let bus = u8::from_str_radix(parts[1], 16).ok()?;
        let dev_fn: Vec<&str> = parts[2].split('.').collect();
        if dev_fn.len() != 2 {
            return None;
        }
        let device = u8::from_str_radix(dev_fn[0], 16).ok()?;
        let function = u8::from_str_radix(dev_fn[1], 16).ok()?;
        Some(Self {
            domain,
            bus,
            device,
            function,
        })
    }
}

impl std::fmt::Display for PciAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:04x}:{:02x}:{:02x}.{:x}",
            self.domain, self.bus, self.device, self.function
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pci_address() {
        let addr = PciAddress::parse("0000:41:00.0").unwrap();
        assert_eq!(addr.domain, 0);
        assert_eq!(addr.bus, 0x41);
        assert_eq!(addr.device, 0);
        assert_eq!(addr.function, 0);
        assert_eq!(addr.to_string(), "0000:41:00.0");
    }

    #[test]
    fn parse_pci_address_invalid() {
        assert!(PciAddress::parse("invalid").is_none());
        assert!(PciAddress::parse("0000:41:00").is_none());
    }
}
