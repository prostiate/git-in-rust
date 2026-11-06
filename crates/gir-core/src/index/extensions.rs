use crate::hash::ObjectId;

/// An optional extension chunk stored at the end of `.git/index` (`TREE`, `REUC`, `UNTR`, etc.).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexExtension {
    pub signature: [u8; 4],
    pub data: Vec<u8>,
}

impl IndexExtension {
    /// Returns `true` if this is the `TREE` (cache-tree) extension.
    #[inline]
    pub fn is_cache_tree(&self) -> bool {
        &self.signature == b"TREE"
    }

    /// Returns `true` if this is the `REUC` (resolve-undo) extension.
    #[inline]
    pub fn is_resolve_undo(&self) -> bool {
        &self.signature == b"REUC"
    }

    /// Returns `true` if the extension signature starts with an uppercase ASCII letter (`'A'..='Z'`),
    /// which in Git's DIRC specification indicates an optional extension that can be safely dropped.
    #[inline]
    pub fn is_optional(&self) -> bool {
        self.signature[0].is_ascii_uppercase()
    }
}

/// Encodes a single `REUC` (Resolve-Undo) record for `path` given its `[stage1, stage2, stage3]` `(mode, oid)` pairs.
pub fn encode_reuc_record(path: &str, stages: [Option<(u32, ObjectId)>; 3]) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(path.as_bytes());
    buf.push(0);
    for st in &stages {
        let mode_val = st.map(|(m, _)| m).unwrap_or(0);
        buf.extend_from_slice(format!("{mode_val:o}\0").as_bytes());
    }
    for st in &stages {
        if let Some((mode_val, oid)) = st {
            if *mode_val != 0 {
                buf.extend_from_slice(oid.as_bytes());
            }
        }
    }
    buf
}
