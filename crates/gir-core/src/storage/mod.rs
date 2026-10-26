pub mod atomic;
pub mod fsck;
pub mod loose;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::error::GirError;
use crate::hash::ObjectId;
use crate::object::{ObjectKind, ParsedObject, RawObject};
use crate::pack::PackBundle;
use loose::LooseObjectStore;

/// Unified Object Database (`ObjectDatabase`) coordinating loose objects and `.pack`/`.idx` bundles.
#[derive(Debug, Clone)]
pub struct ObjectDatabase {
    objects_dir: PathBuf,
    loose: LooseObjectStore,
    packs: Arc<Mutex<Vec<PackBundle>>>,
}

impl ObjectDatabase {
    pub fn new(objects_dir: impl Into<PathBuf>) -> Self {
        let dir = objects_dir.into();
        let packs = discover_pack_bundles(&dir.join("pack"));
        Self {
            loose: LooseObjectStore::new(dir.clone()),
            objects_dir: dir,
            packs: Arc::new(Mutex::new(packs)),
        }
    }

    pub fn objects_dir(&self) -> &Path {
        &self.objects_dir
    }

    pub fn pack_dir(&self) -> PathBuf {
        self.objects_dir.join("pack")
    }

    pub fn loose(&self) -> &LooseObjectStore {
        &self.loose
    }

    /// Re-scans `.git/objects/pack/` to pick up newly created or fetched packfiles.
    pub fn refresh_packs(&self) -> Vec<PackBundle> {
        let discovered = discover_pack_bundles(&self.pack_dir());
        if let Ok(mut guard) = self.packs.lock() {
            *guard = discovered.clone();
        }
        discovered
    }

    /// Returns a snapshot of currently loaded `PackBundle`s.
    pub fn packs(&self) -> Vec<PackBundle> {
        self.packs
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// Returns `true` if `id` exists in either loose storage or any packfile.
    pub fn exists(&self, id: &ObjectId) -> bool {
        if self.loose.exists(id) {
            return true;
        }
        for bundle in self.packs() {
            if bundle.contains(id) {
                return true;
            }
        }
        for bundle in self.refresh_packs() {
            if bundle.contains(id) {
                return true;
            }
        }
        false
    }

    /// Writes a raw object (`<kind> <size>\0<payload>`) atomically to the loose object store.
    pub fn write_raw(&self, kind: ObjectKind, payload: &[u8]) -> Result<ObjectId, GirError> {
        self.loose.write_raw(kind, payload)
    }

    /// Writes a `RawObject` atomically to the loose object store.
    pub fn write_object(&self, raw: &RawObject) -> Result<ObjectId, GirError> {
        self.loose.write_object(raw)
    }

    /// Reads an uncompressed `RawObject` by its exact `ObjectId` from loose or packed storage.
    pub fn read_raw(&self, id: &ObjectId) -> Result<RawObject, GirError> {
        if self.loose.exists(id) {
            return self.loose.read_raw(id);
        }

        if let Some(raw) = self.read_from_packs(&self.packs(), id)? {
            return Ok(raw);
        }

        // Re-scan `.git/objects/pack/` once in case a concurrent `git fetch` or `gc` added a pack
        let refreshed = self.refresh_packs();
        if let Some(raw) = self.read_from_packs(&refreshed, id)? {
            return Ok(raw);
        }

        Err(GirError::ObjectNotFound(id.to_hex()))
    }

    fn read_from_packs(
        &self,
        bundles: &[PackBundle],
        id: &ObjectId,
    ) -> Result<Option<RawObject>, GirError> {
        for bundle in bundles {
            if let Some(raw) = bundle.read_object(id, Some(self))? {
                let actual_id = raw.id();
                if &actual_id != id {
                    return Err(GirError::HashMismatch {
                        expected: id.to_hex(),
                        actual: actual_id.to_hex(),
                    });
                }
                return Ok(Some(raw));
            }
        }
        Ok(None)
    }

    /// Reads and parses a strongly-typed `ParsedObject` by its `ObjectId`.
    pub fn read_parsed(&self, id: &ObjectId) -> Result<ParsedObject, GirError> {
        let raw = self.read_raw(id)?;
        raw.parse(id)
    }

    /// Resolves a full 40-char hex SHA or unambiguous 4..39-char hex prefix into an `ObjectId`
    /// across both loose objects and all `.idx` v2 pack indices.
    pub fn resolve_prefix(&self, prefix: &str) -> Result<ObjectId, GirError> {
        let mut matches = self.loose.resolve_prefix(prefix)?;
        let bundles = self.refresh_packs();
        for bundle in &bundles {
            let pack_matches = bundle.index.resolve_prefix(prefix)?;
            matches.extend(pack_matches);
        }

        matches.sort();
        matches.dedup();

        match matches.len() {
            0 => Err(GirError::ObjectNotFound(prefix.to_string())),
            1 => Ok(matches[0]),
            count => Err(GirError::AmbiguousObjectPrefix {
                prefix: prefix.to_string(),
                count,
            }),
        }
    }

    /// Computes the shortest unambiguous hexadecimal prefix (at least `min_len`, minimum 4) for `id`.
    pub fn abbreviate_oid(&self, id: &ObjectId, min_len: usize) -> String {
        let full = id.to_hex();
        let start = min_len.clamp(4, ObjectId::HEX_LEN);
        for len in start..ObjectId::HEX_LEN {
            let candidate = &full[..len];
            if let Ok(resolved) = self.resolve_prefix(candidate) {
                if &resolved == id {
                    return candidate.to_string();
                }
            }
        }
        full
    }

    /// Returns a sorted, deduplicated list of all `ObjectId`s present in loose and packed storage.
    pub fn list_all_objects(&self) -> Result<Vec<ObjectId>, GirError> {
        let mut ids = self.loose.list_all()?;
        for bundle in self.refresh_packs() {
            for entry in bundle.index.entries()? {
                ids.push(entry.oid);
            }
        }
        ids.sort();
        ids.dedup();
        Ok(ids)
    }
}

fn discover_pack_bundles(pack_dir: &Path) -> Vec<PackBundle> {
    let Ok(read_dir) = fs::read_dir(pack_dir) else {
        return Vec::new();
    };

    let mut idx_paths: Vec<PathBuf> = read_dir
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("idx"))
        .collect();
    idx_paths.sort();

    let mut bundles = Vec::with_capacity(idx_paths.len());
    for idx_path in idx_paths {
        if let Ok(bundle) = PackBundle::open_from_idx(&idx_path) {
            bundles.push(bundle);
        }
    }
    bundles
}
