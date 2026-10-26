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

    /// Returns `true` if the extension signature starts with an uppercase ASCII letter (`'A'..='Z'`),
    /// which in Git's DIRC specification indicates an optional extension that can be safely dropped.
    #[inline]
    pub fn is_optional(&self) -> bool {
        self.signature[0].is_ascii_uppercase()
    }
}
