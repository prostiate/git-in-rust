use std::cmp::Ordering;
use std::fmt;

use crate::error::GirError;
use crate::hash::ObjectId;
use crate::object::{ObjectKind, RawObject};

/// Git tree entry file mode.
///
/// Note on Git's binary `tree` serialization:
/// - In `.git/objects` tree payloads, directory mode is serialized as 5 ASCII characters `"40000"`
///   (no leading `'0'`), while in `git ls-tree` display output it is padded to 6 characters `"040000"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileMode {
    /// Regular non-executable file (`100644`).
    Regular,
    /// Regular executable file (`100755`).
    Executable,
    /// Symbolic link (`120000`).
    Symlink,
    /// Subdirectory tree (`40000` in binary tree payload, `040000` in display).
    Directory,
    /// Submodule gitlink (`160000`).
    Gitlink,
}

impl FileMode {
    /// Parses the ASCII octal mode string from a raw `tree` object payload (`"100644"`, `"40000"`, etc.),
    /// including legacy Git mode normalization (e.g., `"100664"` -> `Regular`).
    pub fn from_tree_octal(s: &str) -> Result<Self, GirError> {
        match s {
            "100644" | "0100644" | "100664" | "0100664" => Ok(Self::Regular),
            "100755" | "0100755" => Ok(Self::Executable),
            "120000" | "0120000" => Ok(Self::Symlink),
            "40000" | "040000" => Ok(Self::Directory),
            "160000" | "0160000" => Ok(Self::Gitlink),
            other => {
                if let Ok(bits) = u32::from_str_radix(other, 8) {
                    match bits & 0o170000 {
                        0o040000 => return Ok(Self::Directory),
                        0o120000 => return Ok(Self::Symlink),
                        0o160000 => return Ok(Self::Gitlink),
                        0o100000 => {
                            return if (bits & 0o111) != 0 {
                                Ok(Self::Executable)
                            } else {
                                Ok(Self::Regular)
                            };
                        }
                        _ => {}
                    }
                }
                Err(GirError::InvalidArgument(format!(
                    "invalid tree entry file mode '{other}'"
                )))
            }
        }
    }

    /// Returns the exact ASCII octal string written into a binary `tree` object payload
    /// (notably `"40000"` without a leading zero for directories).
    pub const fn as_tree_octal(&self) -> &'static str {
        match self {
            Self::Regular => "100644",
            Self::Executable => "100755",
            Self::Symlink => "120000",
            Self::Directory => "40000",
            Self::Gitlink => "160000",
        }
    }

    /// Returns the 6-character zero-padded octal string used by `git ls-tree` (`"040000"` for directories).
    pub const fn as_padded_octal(&self) -> &'static str {
        match self {
            Self::Regular => "100644",
            Self::Executable => "100755",
            Self::Symlink => "120000",
            Self::Directory => "040000",
            Self::Gitlink => "160000",
        }
    }

    /// Returns the object type (`"blob"`, `"tree"`, or `"commit"`) corresponding to this entry mode.
    pub const fn object_type_str(&self) -> &'static str {
        match self {
            Self::Regular | Self::Executable | Self::Symlink => "blob",
            Self::Directory => "tree",
            Self::Gitlink => "commit",
        }
    }

    #[inline]
    pub const fn is_dir(&self) -> bool {
        matches!(self, Self::Directory)
    }
}

impl fmt::Display for FileMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_padded_octal())
    }
}

/// A single entry inside a Git `tree` object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    pub mode: FileMode,
    pub name: String,
    pub oid: ObjectId,
}

impl TreeEntry {
    pub fn new(mode: FileMode, name: impl Into<String>, oid: ObjectId) -> Self {
        Self {
            mode,
            name: name.into(),
            oid,
        }
    }
}

/// Git's canonical tree entry comparator (`base_name_compare` in `tree.c`):
///
/// Entries are compared byte-by-byte, EXCEPT directory entries (`FileMode::Directory`)
/// are compared as if their name had an implicit trailing `'/'` (`0x2F`).
/// For example: `"foo-bar"` (`'-' == 0x2D`) < `"foo.txt"` (`'.' == 0x2E`) < `"foo"` (dir, `'/' == 0x2F`) < `"foo0"` (`'0' == 0x30`).
pub fn tree_entry_cmp(a: &TreeEntry, b: &TreeEntry) -> Ordering {
    let a_bytes = a.name.as_bytes();
    let b_bytes = b.name.as_bytes();
    let min_len = a_bytes.len().min(b_bytes.len());

    match a_bytes[..min_len].cmp(&b_bytes[..min_len]) {
        Ordering::Equal => {
            let next_a = if a_bytes.len() > min_len {
                Some(a_bytes[min_len])
            } else if a.mode.is_dir() {
                Some(b'/')
            } else {
                None
            };

            let next_b = if b_bytes.len() > min_len {
                Some(b_bytes[min_len])
            } else if b.mode.is_dir() {
                Some(b'/')
            } else {
                None
            };

            next_a.cmp(&next_b)
        }
        non_eq => non_eq,
    }
}

/// A Git `tree` object representing a single directory snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Tree {
    pub entries: Vec<TreeEntry>,
}

impl Tree {
    /// Creates a new `Tree`, automatically sorting entries using Git's directory-aware comparator.
    pub fn new(mut entries: Vec<TreeEntry>) -> Self {
        entries.sort_by(tree_entry_cmp);
        Self { entries }
    }

    /// Parses a raw binary `tree` payload (`<mode> <name>\0<20-byte-sha>...`).
    pub fn parse(id_hint: &str, mut data: &[u8]) -> Result<Self, GirError> {
        let mut entries = Vec::new();

        while !data.is_empty() {
            let space_pos =
                data.iter()
                    .position(|&b| b == b' ')
                    .ok_or_else(|| GirError::CorruptObject {
                        id: id_hint.to_string(),
                        reason: "missing space after mode in tree entry".to_string(),
                    })?;

            let mode_str =
                std::str::from_utf8(&data[..space_pos]).map_err(|_| GirError::CorruptObject {
                    id: id_hint.to_string(),
                    reason: "non-UTF8 mode in tree entry".to_string(),
                })?;
            let mode =
                FileMode::from_tree_octal(mode_str).map_err(|_| GirError::CorruptObject {
                    id: id_hint.to_string(),
                    reason: format!("invalid file mode '{mode_str}' in tree"),
                })?;

            data = &data[space_pos + 1..];

            let nul_pos =
                data.iter()
                    .position(|&b| b == 0)
                    .ok_or_else(|| GirError::CorruptObject {
                        id: id_hint.to_string(),
                        reason: "missing NUL terminator after filename in tree entry".to_string(),
                    })?;

            let name =
                std::str::from_utf8(&data[..nul_pos]).map_err(|_| GirError::CorruptObject {
                    id: id_hint.to_string(),
                    reason: "non-UTF8 filename in tree entry".to_string(),
                })?;

            data = &data[nul_pos + 1..];

            if data.len() < ObjectId::BYTE_LEN {
                return Err(GirError::CorruptObject {
                    id: id_hint.to_string(),
                    reason: "truncated 20-byte SHA-1 in tree entry".to_string(),
                });
            }

            let oid = ObjectId::from_slice(&data[..ObjectId::BYTE_LEN])?;
            data = &data[ObjectId::BYTE_LEN..];

            entries.push(TreeEntry {
                mode,
                name: name.to_string(),
                oid,
            });
        }

        Ok(Self { entries })
    }

    /// Serializes this `Tree` into its canonical binary payload (`<mode> <name>\0<20-byte-sha>`).
    pub fn serialize(&self) -> Vec<u8> {
        let mut sorted = self.entries.clone();
        sorted.sort_by(tree_entry_cmp);

        let mut buf = Vec::new();
        for entry in &sorted {
            buf.extend_from_slice(entry.mode.as_tree_octal().as_bytes());
            buf.push(b' ');
            buf.extend_from_slice(entry.name.as_bytes());
            buf.push(0);
            buf.extend_from_slice(entry.oid.as_bytes());
        }
        buf
    }

    /// Converts this `Tree` into a `RawObject`.
    pub fn to_raw(&self) -> RawObject {
        RawObject {
            kind: ObjectKind::Tree,
            data: self.serialize(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_tree_directory_sorting_order() {
        let dummy = ObjectId::ZERO;
        let entries = vec![
            TreeEntry::new(FileMode::Directory, "foo", dummy),
            TreeEntry::new(FileMode::Regular, "foo.txt", dummy),
            TreeEntry::new(FileMode::Regular, "foo-bar", dummy),
            TreeEntry::new(FileMode::Regular, "foo0", dummy),
        ];

        let tree = Tree::new(entries);
        let names: Vec<(&str, FileMode)> = tree
            .entries
            .iter()
            .map(|e| (e.name.as_str(), e.mode))
            .collect();

        // '-' (0x2D) < '.' (0x2E) < '/' (0x2F, implicit for dir "foo") < '0' (0x30)
        assert_eq!(
            names,
            vec![
                ("foo-bar", FileMode::Regular),
                ("foo.txt", FileMode::Regular),
                ("foo", FileMode::Directory),
                ("foo0", FileMode::Regular),
            ]
        );
    }

    #[test]
    fn tree_roundtrip_serialization() {
        let oid1 = ObjectId::from_hex("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391").unwrap();
        let oid2 = ObjectId::from_hex("4b825dc642cb6eb9a060e54bf8d69288fbee4904").unwrap();

        let tree = Tree::new(vec![
            TreeEntry::new(FileMode::Directory, "src", oid2),
            TreeEntry::new(FileMode::Regular, "README.md", oid1),
        ]);

        let bytes = tree.serialize();
        let decoded = Tree::parse("test", &bytes).unwrap();
        assert_eq!(decoded, tree);
    }
}
