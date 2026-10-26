use std::fmt;
use std::str::FromStr;

use sha1::{Digest, Sha1};

use crate::error::GirError;

/// A 20-byte SHA-1 content-addressable Git object identifier.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectId([u8; 20]);

impl ObjectId {
    /// The all-zero null SHA-1 (`0000000000000000000000000000000000000000`).
    pub const ZERO: Self = Self([0u8; 20]);

    /// Raw byte length of a SHA-1 object ID.
    pub const BYTE_LEN: usize = 20;

    /// Hexadecimal string length of a SHA-1 object ID.
    pub const HEX_LEN: usize = 40;

    /// Creates an `ObjectId` from raw 20-byte slice.
    #[inline]
    pub const fn from_bytes(bytes: [u8; 20]) -> Self {
        Self(bytes)
    }

    /// Attempts to construct an `ObjectId` from a byte slice of length 20.
    #[inline]
    pub fn from_slice(slice: &[u8]) -> Result<Self, GirError> {
        let arr: [u8; 20] = slice
            .try_into()
            .map_err(|_| GirError::InvalidObjectId(format!("<{} raw bytes>", slice.len())))?;
        Ok(Self(arr))
    }

    /// Parses a 40-character hexadecimal string into an `ObjectId`.
    pub fn from_hex(hex: &str) -> Result<Self, GirError> {
        let hex = hex.trim();
        if hex.len() != Self::HEX_LEN {
            return Err(GirError::InvalidObjectId(hex.to_string()));
        }

        let mut bytes = [0u8; 20];
        let raw = hex.as_bytes();
        for i in 0..20 {
            let hi = decode_hex_nibble(raw[i * 2])
                .ok_or_else(|| GirError::InvalidObjectId(hex.to_string()))?;
            let lo = decode_hex_nibble(raw[i * 2 + 1])
                .ok_or_else(|| GirError::InvalidObjectId(hex.to_string()))?;
            bytes[i] = (hi << 4) | lo;
        }
        Ok(Self(bytes))
    }

    /// Computes a Git object SHA-1 from its `<kind> <len>\0` header and payload bytes.
    pub fn hash_object(kind_str: &str, payload: &[u8]) -> Self {
        let mut hasher = Sha1::new();
        let header = format!("{} {}\0", kind_str, payload.len());
        hasher.update(header.as_bytes());
        hasher.update(payload);
        let digest = hasher.finalize();
        let mut bytes = [0u8; 20];
        bytes.copy_from_slice(&digest);
        Self(bytes)
    }

    /// Returns a reference to the underlying 20-byte array.
    #[inline]
    pub const fn as_bytes(&self) -> &[u8; 20] {
        &self.0
    }

    /// Returns true if this is the all-zero `ObjectId::ZERO`.
    #[inline]
    pub fn is_zero(&self) -> bool {
        self.0 == [0u8; 20]
    }

    /// Formats this `ObjectId` as a 40-character lowercase hexadecimal string.
    pub fn to_hex(&self) -> String {
        const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";
        let mut buf = [0u8; 40];
        for (i, &b) in self.0.iter().enumerate() {
            buf[i * 2] = HEX_CHARS[(b >> 4) as usize];
            buf[i * 2 + 1] = HEX_CHARS[(b & 0x0F) as usize];
        }
        // Safety: `buf` contains exclusively ASCII hex characters.
        String::from_utf8(buf.to_vec()).expect("valid ASCII hex")
    }

    /// Returns the 7-character short hexadecimal prefix of this `ObjectId`.
    pub fn short_hex(&self) -> String {
        self.to_hex()[..7].to_string()
    }

    /// Splits the 40-character hex representation into `(first_2_chars, remaining_38_chars)`
    /// for `.git/objects/xx/yyyy...` loose object storage.
    pub fn fanout_prefix(&self) -> (String, String) {
        let hex = self.to_hex();
        (hex[..2].to_string(), hex[2..].to_string())
    }

    /// Checks whether this `ObjectId`'s lowercase hex representation starts with `prefix`.
    pub fn starts_with_hex(&self, prefix: &str) -> bool {
        let prefix = prefix.trim().to_ascii_lowercase();
        self.to_hex().starts_with(&prefix)
    }
}

#[inline]
fn decode_hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Validates that a string is a valid hexadecimal object prefix (4..=40 hex chars).
pub fn validate_hex_prefix(prefix: &str) -> Result<String, GirError> {
    let trimmed = prefix.trim();
    if trimmed.len() < 4 || trimmed.len() > 40 {
        return Err(GirError::InvalidObjectPrefix(trimmed.to_string()));
    }
    if !trimmed.bytes().all(|b| decode_hex_nibble(b).is_some()) {
        return Err(GirError::InvalidObjectPrefix(trimmed.to_string()));
    }
    Ok(trimmed.to_ascii_lowercase())
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ObjectId({})", self.to_hex())
    }
}

impl FromStr for ObjectId {
    type Err = GirError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_hex(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_hex_and_fanout() {
        let hex = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";
        let oid = ObjectId::from_hex(hex).expect("valid sha1");
        assert_eq!(oid.to_hex(), hex);
        assert_eq!(oid.short_hex(), "e69de29");
        let (dir, file) = oid.fanout_prefix();
        assert_eq!(dir, "e6");
        assert_eq!(file, "9de29bb2d1d6434b8b29ae775ad8c2e48c5391");
    }

    #[test]
    fn empty_blob_sha1_matches_git() {
        // In Git, an empty blob ("blob 0\0") always hashes to e69de29bb2d1d6434b8b29ae775ad8c2e48c5391
        let oid = ObjectId::hash_object("blob", b"");
        assert_eq!(oid.to_hex(), "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391");
    }

    #[test]
    fn rejects_invalid_hex() {
        assert!(ObjectId::from_hex("short").is_err());
        assert!(ObjectId::from_hex("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz").is_err());
    }
}
