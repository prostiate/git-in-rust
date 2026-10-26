pub mod atomic;
pub mod loose;

use std::path::PathBuf;

use crate::error::GirError;
use crate::hash::ObjectId;
use crate::object::{ObjectKind, ParsedObject, RawObject};
use loose::LooseObjectStore;

/// Unified Object Database (`ObjectDatabase`) coordinating loose objects and packfiles.
#[derive(Debug, Clone)]
pub struct ObjectDatabase {
    loose: LooseObjectStore,
}

impl ObjectDatabase {
    pub fn new(objects_dir: impl Into<PathBuf>) -> Self {
        let dir = objects_dir.into();
        Self {
            loose: LooseObjectStore::new(dir),
        }
    }

    pub fn loose(&self) -> &LooseObjectStore {
        &self.loose
    }

    /// Returns `true` if `id` exists in the object database.
    pub fn exists(&self, id: &ObjectId) -> bool {
        self.loose.exists(id)
    }

    /// Writes a raw object (`<kind> <size>\0<payload>`) atomically to the loose object store.
    pub fn write_raw(&self, kind: ObjectKind, payload: &[u8]) -> Result<ObjectId, GirError> {
        self.loose.write_raw(kind, payload)
    }

    /// Writes a `RawObject` atomically to the loose object store.
    pub fn write_object(&self, raw: &RawObject) -> Result<ObjectId, GirError> {
        self.loose.write_object(raw)
    }

    /// Reads an uncompressed `RawObject` by its exact `ObjectId`.
    pub fn read_raw(&self, id: &ObjectId) -> Result<RawObject, GirError> {
        self.loose.read_raw(id)
    }

    /// Reads and parses a strongly-typed `ParsedObject` by its `ObjectId`.
    pub fn read_parsed(&self, id: &ObjectId) -> Result<ParsedObject, GirError> {
        let raw = self.read_raw(id)?;
        raw.parse(id)
    }

    /// Resolves a full 40-char hex SHA or unambiguous 4..39-char hex prefix into an `ObjectId`.
    pub fn resolve_prefix(&self, prefix: &str) -> Result<ObjectId, GirError> {
        let matches = self.loose.resolve_prefix(prefix)?;
        match matches.len() {
            0 => Err(GirError::ObjectNotFound(prefix.to_string())),
            1 => Ok(matches[0]),
            count => Err(GirError::AmbiguousObjectPrefix {
                prefix: prefix.to_string(),
                count,
            }),
        }
    }
}
