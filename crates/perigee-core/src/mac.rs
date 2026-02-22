use crate::error::{PerigeeError, Result};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MacAddress(pub [u8; 6]);

impl MacAddress {
    pub const ZERO: Self = Self([0; 6]);

    pub fn new(bytes: [u8; 6]) -> Self {
        Self(bytes)
    }

    pub fn is_zero(&self) -> bool {
        self.0 == [0; 6]
    }

    pub fn is_unicast(&self) -> bool {
        self.0[0] & 0x01 == 0
    }

    pub fn is_locally_administered(&self) -> bool {
        self.0[0] & 0x02 != 0
    }

    /// Increment MAC address by a given offset.
    /// Handles carry across all bytes.
    pub fn increment(&self, offset: u64) -> Self {
        let mut value: u64 = 0;
        for &b in &self.0 {
            value = (value << 8) | b as u64;
        }
        value = value.wrapping_add(offset);
        let mut bytes = [0u8; 6];
        for i in (0..6).rev() {
            bytes[i] = (value & 0xFF) as u8;
            value >>= 8;
        }
        Self(bytes)
    }

    /// Generate a random locally-administered unicast MAC address.
    pub fn random_local() -> Self {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        let mut bytes = [0u8; 6];
        rng.fill(&mut bytes);
        // Set locally administered bit (bit 1), clear multicast bit (bit 0)
        bytes[0] = (bytes[0] | 0x02) & 0xFE;
        Self(bytes)
    }

    /// Generate a random locally-administered unicast MAC with a specific seed.
    pub fn random_local_seeded(seed: u64) -> Self {
        use rand::rngs::StdRng;
        use rand::{Rng, SeedableRng};
        let mut rng = StdRng::seed_from_u64(seed);
        let mut bytes = [0u8; 6];
        rng.fill(&mut bytes);
        bytes[0] = (bytes[0] | 0x02) & 0xFE;
        Self(bytes)
    }

    pub fn bytes(&self) -> &[u8; 6] {
        &self.0
    }
}

impl fmt::Display for MacAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            self.0[0], self.0[1], self.0[2], self.0[3], self.0[4], self.0[5]
        )
    }
}

impl FromStr for MacAddress {
    type Err = PerigeeError;

    fn from_str(s: &str) -> Result<Self> {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() != 6 {
            return Err(PerigeeError::Mac(format!("invalid MAC format: {}", s)));
        }
        let mut bytes = [0u8; 6];
        for (i, part) in parts.iter().enumerate() {
            bytes[i] = u8::from_str_radix(part, 16)
                .map_err(|_| PerigeeError::Mac(format!("invalid MAC octet '{}' in {}", part, s)))?;
        }
        Ok(Self(bytes))
    }
}

impl Serialize for MacAddress {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for MacAddress {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mac() {
        let mac: MacAddress = "b8:ce:f6:12:34:56".parse().unwrap();
        assert_eq!(mac.0, [0xb8, 0xce, 0xf6, 0x12, 0x34, 0x56]);
        assert_eq!(mac.to_string(), "b8:ce:f6:12:34:56");
    }

    #[test]
    fn increment_mac() {
        let mac: MacAddress = "b8:ce:f6:12:34:56".parse().unwrap();
        let next = mac.increment(1);
        assert_eq!(next.to_string(), "b8:ce:f6:12:34:57");
    }

    #[test]
    fn increment_mac_carry() {
        let mac: MacAddress = "b8:ce:f6:12:34:ff".parse().unwrap();
        let next = mac.increment(1);
        assert_eq!(next.to_string(), "b8:ce:f6:12:35:00");
    }

    #[test]
    fn random_local_mac_properties() {
        let mac = MacAddress::random_local();
        assert!(mac.is_unicast());
        assert!(mac.is_locally_administered());
    }

    #[test]
    fn mac_serde_roundtrip() {
        let mac: MacAddress = "aa:bb:cc:dd:ee:ff".parse().unwrap();
        let json = serde_json::to_string(&mac).unwrap();
        assert_eq!(json, "\"aa:bb:cc:dd:ee:ff\"");
        let decoded: MacAddress = serde_json::from_str(&json).unwrap();
        assert_eq!(mac, decoded);
    }
}
