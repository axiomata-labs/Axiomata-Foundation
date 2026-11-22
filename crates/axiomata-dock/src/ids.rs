//! The ids that name nodes and tabs: every operation addresses them by id, so each is unique in its tree.

use std::fmt::{self, Write as _};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// A node's id: a group's or a split's. Made by the layout itself ([`NodeId::fresh`]), or read back from a stored one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(transparent)]
pub struct NodeId(String);

/// A tab's id, given by whoever opens the pane (an agent's id, a terminal's instance id …).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(transparent)]
pub struct TabId(String);

impl NodeId {
    /// A new id no other node has: a random UUID in its usual form, as the layouts already stored carry.
    #[must_use]
    pub fn fresh() -> Self {
        Self(random_uuid())
    }

    /// The id as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TabId {
    /// The id as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for NodeId {
    fn from(id: &str) -> Self {
        Self(id.to_owned())
    }
}

impl From<String> for NodeId {
    fn from(id: String) -> Self {
        Self(id)
    }
}

impl From<&str> for TabId {
    fn from(id: &str) -> Self {
        Self(id.to_owned())
    }
}

impl From<String> for TabId {
    fn from(id: String) -> Self {
        Self(id)
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Display for TabId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A version-4 UUID. Should the system's randomness fail, the time and a counter stand in: an id must be unique in
/// one tree, and the counter alone does not repeat for 2⁴⁸ ids within a process.
fn random_uuid() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let count = COUNTER.fetch_add(1, Ordering::Relaxed);
        // Kept clear of bytes 6 and 8, which the version and variant overwrite: the low six bytes of each.
        bytes[..6].copy_from_slice(&nanos.to_le_bytes()[..6]);
        bytes[10..].copy_from_slice(&count.to_le_bytes()[..6]);
    }
    // Version 4, variant 10 (RFC 9562).
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = bytes
        .iter()
        .fold(String::with_capacity(32), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        });
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_id_is_a_uuid_and_new_every_time() {
        let a = NodeId::fresh();
        let b = NodeId::fresh();
        assert_ne!(a, b);
        let parts: Vec<usize> = a.as_str().split('-').map(str::len).collect();
        assert_eq!(parts, [8, 4, 4, 4, 12]);
        assert_eq!(a.as_str().as_bytes()[14], b'4', "version 4");
    }
}
