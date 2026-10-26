use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;

use crate::error::GirError;
use crate::hash::ObjectId;
use crate::storage::atomic::LockFile;

/// A single entry in `.git/packed-refs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackedRefEntry {
    pub oid: ObjectId,
    pub peeled: Option<ObjectId>,
}

/// Parser and atomic writer for `.git/packed-refs`.
#[derive(Debug, Clone, Default)]
pub struct PackedRefs {
    entries: BTreeMap<String, PackedRefEntry>,
}

impl PackedRefs {
    pub fn load(git_dir: &Path) -> Result<Self, GirError> {
        let path = git_dir.join("packed-refs");
        if !path.is_file() {
            return Ok(Self::default());
        }

        let content = fs::read_to_string(&path)?;
        let mut entries: BTreeMap<String, PackedRefEntry> = BTreeMap::new();
        let mut last_ref: Option<String> = None;

        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(peeled_hex) = line.strip_prefix('^') {
                if let (Some(ref prev_name), Ok(peeled_oid)) =
                    (&last_ref, ObjectId::from_hex(peeled_hex.trim()))
                {
                    if let Some(entry) = entries.get_mut(prev_name) {
                        entry.peeled = Some(peeled_oid);
                    }
                }
                continue;
            }

            if let Some((sha_str, ref_name)) = line.split_once(' ') {
                let oid = ObjectId::from_hex(sha_str.trim())?;
                let name = ref_name.trim().to_string();
                entries.insert(name.clone(), PackedRefEntry { oid, peeled: None });
                last_ref = Some(name);
            }
        }

        Ok(Self { entries })
    }

    pub fn get(&self, ref_name: &str) -> Option<&PackedRefEntry> {
        self.entries.get(ref_name)
    }

    pub fn entries(&self) -> &BTreeMap<String, PackedRefEntry> {
        &self.entries
    }

    pub fn insert(&mut self, ref_name: String, oid: ObjectId, peeled: Option<ObjectId>) {
        self.entries.insert(ref_name, PackedRefEntry { oid, peeled });
    }

    pub fn remove(&mut self, ref_name: &str) -> Option<PackedRefEntry> {
        self.entries.remove(ref_name)
    }

    pub fn save(&self, git_dir: &Path) -> Result<(), GirError> {
        let path = git_dir.join("packed-refs");
        let mut lock = LockFile::acquire(&path)?;
        writeln!(lock, "# pack-refs with: peeled fully-peeled sorted")?;
        for (name, entry) in &self.entries {
            writeln!(lock, "{} {}", entry.oid, name)?;
            if let Some(peeled) = entry.peeled {
                writeln!(lock, "^{peeled}")?;
            }
        }
        lock.commit()
    }
}
