use std::str::FromStr;

use crate::error::GirError;
use crate::hash::ObjectId;
use crate::object::commit::Signature;
use crate::object::{ObjectKind, RawObject};

/// An annotated Git `tag` object pointing to another object (typically a `Commit`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    pub object: ObjectId,
    pub object_kind: ObjectKind,
    pub tag_name: String,
    pub tagger: Option<Signature>,
    pub message: String,
}

impl Tag {
    /// Parses a raw `tag` payload.
    pub fn parse(id_hint: &str, data: &[u8]) -> Result<Self, GirError> {
        let text = std::str::from_utf8(data).map_err(|_| GirError::CorruptObject {
            id: id_hint.to_string(),
            reason: "tag object is not valid UTF-8".to_string(),
        })?;

        let (header_block, message) = text.split_once("\n\n").unwrap_or((text, ""));

        let mut object: Option<ObjectId> = None;
        let mut object_kind: Option<ObjectKind> = None;
        let mut tag_name: Option<String> = None;
        let mut tagger: Option<Signature> = None;

        for line in header_block.lines() {
            if let Some(rest) = line.strip_prefix("object ") {
                object = Some(ObjectId::from_hex(rest.trim())?);
            } else if let Some(rest) = line.strip_prefix("type ") {
                object_kind = Some(ObjectKind::from_str(rest.trim())?);
            } else if let Some(rest) = line.strip_prefix("tag ") {
                tag_name = Some(rest.trim().to_string());
            } else if let Some(rest) = line.strip_prefix("tagger ") {
                tagger = Some(Signature::parse(id_hint, rest)?);
            }
        }

        Ok(Self {
            object: object.ok_or_else(|| GirError::CorruptObject {
                id: id_hint.to_string(),
                reason: "tag missing 'object' header".to_string(),
            })?,
            object_kind: object_kind.ok_or_else(|| GirError::CorruptObject {
                id: id_hint.to_string(),
                reason: "tag missing 'type' header".to_string(),
            })?,
            tag_name: tag_name.ok_or_else(|| GirError::CorruptObject {
                id: id_hint.to_string(),
                reason: "tag missing 'tag' header".to_string(),
            })?,
            tagger,
            message: message.to_string(),
        })
    }

    /// Serializes this `Tag` into its canonical UTF-8 payload.
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = String::new();
        out.push_str(&format!("object {}\n", self.object));
        out.push_str(&format!("type {}\n", self.object_kind));
        out.push_str(&format!("tag {}\n", self.tag_name));
        if let Some(ref tagger) = self.tagger {
            out.push_str(&format!("tagger {}\n", tagger));
        }
        out.push('\n');
        out.push_str(&self.message);
        out.into_bytes()
    }

    /// Converts this `Tag` into a `RawObject`.
    pub fn to_raw(&self) -> RawObject {
        RawObject {
            kind: ObjectKind::Tag,
            data: self.serialize(),
        }
    }
}
