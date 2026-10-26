pub mod blob;
pub mod commit;
pub mod tag;
pub mod tree;

use std::fmt;
use std::str::FromStr;

use crate::error::GirError;
use crate::hash::ObjectId;
use blob::Blob;
use commit::Commit;
use tag::Tag;
use tree::Tree;

/// The four fundamental object types in Git's content-addressable store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObjectKind {
    Blob,
    Tree,
    Commit,
    Tag,
}

impl ObjectKind {
    /// Returns the canonical ASCII header type string (`"blob"`, `"tree"`, `"commit"`, `"tag"`).
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Blob => "blob",
            Self::Tree => "tree",
            Self::Commit => "commit",
            Self::Tag => "tag",
        }
    }
}

impl fmt::Display for ObjectKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ObjectKind {
    type Err = GirError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "blob" => Ok(Self::Blob),
            "tree" => Ok(Self::Tree),
            "commit" => Ok(Self::Commit),
            "tag" => Ok(Self::Tag),
            other => Err(GirError::InvalidArgument(format!(
                "unknown git object type '{other}'"
            ))),
        }
    }
}

/// An uncompressed raw Git object consisting of its `ObjectKind` and payload bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawObject {
    pub kind: ObjectKind,
    pub data: Vec<u8>,
}

impl RawObject {
    /// Computes the 20-byte `ObjectId` (`SHA1("<kind> <len>\0<data>")`) for this raw object.
    pub fn id(&self) -> ObjectId {
        ObjectId::hash_object(self.kind.as_str(), &self.data)
    }

    /// Encodes the full uncompressed loose object envelope (`<kind> <len>\0<data>`).
    pub fn encode_envelope(&self) -> Vec<u8> {
        let header = format!("{} {}\0", self.kind.as_str(), self.data.len());
        let mut buf = Vec::with_capacity(header.len() + self.data.len());
        buf.extend_from_slice(header.as_bytes());
        buf.extend_from_slice(&self.data);
        buf
    }

    /// Decodes a decompressed loose object envelope (`<kind> <len>\0<data>`).
    pub fn decode_envelope(id_hint: &str, bytes: &[u8]) -> Result<Self, GirError> {
        let nul_pos = bytes
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| GirError::CorruptObject {
                id: id_hint.to_string(),
                reason: "missing NUL byte in object header".to_string(),
            })?;

        let header_str =
            std::str::from_utf8(&bytes[..nul_pos]).map_err(|_| GirError::CorruptObject {
                id: id_hint.to_string(),
                reason: "non-UTF8 object header".to_string(),
            })?;

        let (kind_str, size_str) =
            header_str
                .split_once(' ')
                .ok_or_else(|| GirError::CorruptObject {
                    id: id_hint.to_string(),
                    reason: format!("malformed object header '{header_str}'"),
                })?;

        let kind = ObjectKind::from_str(kind_str).map_err(|_| GirError::CorruptObject {
            id: id_hint.to_string(),
            reason: format!("unknown object kind '{kind_str}'"),
        })?;

        let expected_size: usize = size_str.parse().map_err(|_| GirError::CorruptObject {
            id: id_hint.to_string(),
            reason: format!("invalid object size '{size_str}'"),
        })?;

        let payload = &bytes[nul_pos + 1..];
        if payload.len() != expected_size {
            return Err(GirError::CorruptObject {
                id: id_hint.to_string(),
                reason: format!(
                    "payload length mismatch: header declared {expected_size}, got {}",
                    payload.len()
                ),
            });
        }

        Ok(Self {
            kind,
            data: payload.to_vec(),
        })
    }

    /// Parses the raw payload into a strongly-typed `ParsedObject`.
    pub fn parse(&self, id: &ObjectId) -> Result<ParsedObject, GirError> {
        match self.kind {
            ObjectKind::Blob => Ok(ParsedObject::Blob(Blob {
                data: self.data.clone(),
            })),
            ObjectKind::Tree => Ok(ParsedObject::Tree(Tree::parse(&id.to_hex(), &self.data)?)),
            ObjectKind::Commit => Ok(ParsedObject::Commit(Commit::parse(
                &id.to_hex(),
                &self.data,
            )?)),
            ObjectKind::Tag => Ok(ParsedObject::Tag(Tag::parse(&id.to_hex(), &self.data)?)),
        }
    }
}

/// Strongly-typed representation of a parsed Git object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedObject {
    Blob(Blob),
    Tree(Tree),
    Commit(Commit),
    Tag(Tag),
}

impl ParsedObject {
    #[inline]
    pub fn kind(&self) -> ObjectKind {
        match self {
            ParsedObject::Blob(_) => ObjectKind::Blob,
            ParsedObject::Tree(_) => ObjectKind::Tree,
            ParsedObject::Commit(_) => ObjectKind::Commit,
            ParsedObject::Tag(_) => ObjectKind::Tag,
        }
    }
}
