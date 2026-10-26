use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;

use crate::error::GirError;
use crate::hash::{validate_hex_prefix, ObjectId};
use crate::object::{ObjectKind, RawObject};
use crate::storage::atomic::write_immutable_atomic;

/// Manages loose Git objects stored under `.git/objects/xx/yyyy...`.
#[derive(Debug, Clone)]
pub struct LooseObjectStore {
    objects_dir: PathBuf,
}

impl LooseObjectStore {
    pub fn new(objects_dir: impl Into<PathBuf>) -> Self {
        Self {
            objects_dir: objects_dir.into(),
        }
    }

    pub fn objects_dir(&self) -> &Path {
        &self.objects_dir
    }

    /// Returns the filesystem path `.git/objects/xx/yyyy...` for a given `ObjectId`.
    pub fn object_path(&self, id: &ObjectId) -> PathBuf {
        let (fanout, remainder) = id.fanout_prefix();
        self.objects_dir.join(fanout).join(remainder)
    }

    /// Returns `true` if the loose object file exists on disk.
    pub fn exists(&self, id: &ObjectId) -> bool {
        self.object_path(id).is_file()
    }

    /// Writes a `RawObject` to the loose object store atomically (if not already present)
    /// and returns its 20-byte `ObjectId`.
    pub fn write_raw(&self, kind: ObjectKind, payload: &[u8]) -> Result<ObjectId, GirError> {
        let raw = RawObject {
            kind,
            data: payload.to_vec(),
        };
        self.write_object(&raw)
    }

    /// Writes a pre-constructed `RawObject` to the loose object store atomically.
    pub fn write_object(&self, raw: &RawObject) -> Result<ObjectId, GirError> {
        let id = raw.id();
        let target_path = self.object_path(&id);
        if target_path.is_file() {
            return Ok(id);
        }

        let envelope = raw.encode_envelope();
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&envelope)?;
        let compressed = encoder.finish()?;

        write_immutable_atomic(&target_path, &compressed)?;
        Ok(id)
    }

    /// Reads, decompresses, and verifies the SHA-1 integrity of a loose object.
    pub fn read_raw(&self, id: &ObjectId) -> Result<RawObject, GirError> {
        let path = self.object_path(id);
        if !path.is_file() {
            return Err(GirError::ObjectNotFound(id.to_hex()));
        }

        let file = File::open(&path)?;
        let mut decoder = ZlibDecoder::new(file);
        let mut decompressed = Vec::new();
        decoder
            .read_to_end(&mut decompressed)
            .map_err(|e| GirError::CorruptObject {
                id: id.to_hex(),
                reason: format!("zlib decompression failed: {e}"),
            })?;

        let raw = RawObject::decode_envelope(&id.to_hex(), &decompressed)?;
        let computed_id = raw.id();
        if &computed_id != id {
            return Err(GirError::HashMismatch {
                expected: id.to_hex(),
                actual: computed_id.to_hex(),
            });
        }

        Ok(raw)
    }

    /// Resolves a hexadecimal prefix (`4..=40` hex chars) against loose objects in `.git/objects/xx/`.
    pub fn resolve_prefix(&self, hex_prefix: &str) -> Result<Vec<ObjectId>, GirError> {
        let prefix = validate_hex_prefix(hex_prefix)?;
        if prefix.len() == ObjectId::HEX_LEN {
            let oid = ObjectId::from_hex(&prefix)?;
            return if self.exists(&oid) {
                Ok(vec![oid])
            } else {
                Ok(Vec::new())
            };
        }

        let fanout_dir = self.objects_dir.join(&prefix[..2]);
        if !fanout_dir.is_dir() {
            return Ok(Vec::new());
        }

        let rem_prefix = &prefix[2..];
        let mut matches = Vec::new();

        for entry in fs::read_dir(&fanout_dir)? {
            let entry = entry?;
            let file_name = entry.file_name();
            let Some(name_str) = file_name.to_str() else {
                continue;
            };
            if name_str.len() == 38 && name_str.starts_with(rem_prefix) {
                let full_hex = format!("{}{}", &prefix[..2], name_str);
                if let Ok(oid) = ObjectId::from_hex(&full_hex) {
                    matches.push(oid);
                }
            }
        }

        matches.sort();
        matches.dedup();
        Ok(matches)
    }

    /// Enumerates all loose `ObjectId`s stored under `.git/objects/00..ff/`.
    pub fn list_all(&self) -> Result<Vec<ObjectId>, GirError> {
        let mut ids = Vec::new();
        if !self.objects_dir.is_dir() {
            return Ok(ids);
        }
        for dir_entry in fs::read_dir(&self.objects_dir)? {
            let dir_entry = dir_entry?;
            let dir_name = dir_entry.file_name();
            let Some(dir_str) = dir_name.to_str() else {
                continue;
            };
            if dir_str.len() != 2 || !dir_str.bytes().all(|b| b.is_ascii_hexdigit()) {
                continue;
            }
            let sub_dir = dir_entry.path();
            if !sub_dir.is_dir() {
                continue;
            }
            for file_entry in fs::read_dir(&sub_dir)? {
                let file_entry = file_entry?;
                let file_name = file_entry.file_name();
                let Some(file_str) = file_name.to_str() else {
                    continue;
                };
                if file_str.len() == 38 && file_str.bytes().all(|b| b.is_ascii_hexdigit()) {
                    let full_hex = format!("{dir_str}{file_str}");
                    if let Ok(oid) = ObjectId::from_hex(&full_hex) {
                        ids.push(oid);
                    }
                }
            }
        }
        ids.sort();
        ids.dedup();
        Ok(ids)
    }

    /// Removes a loose object file from `.git/objects/xx/yyyy...` (used after packing in `gir gc`).
    pub fn remove(&self, id: &ObjectId) -> Result<(), GirError> {
        let path = self.object_path(id);
        if path.is_file() {
            // Make writable if read-only before unlinking
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(meta) = fs::metadata(&path) {
                    let mut perms = meta.permissions();
                    perms.set_mode(0o644);
                    let _ = fs::set_permissions(&path, perms);
                }
            }
            fs::remove_file(&path)?;
        }
        Ok(())
    }
}
