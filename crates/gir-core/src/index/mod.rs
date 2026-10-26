pub mod codec;
pub mod entry;
pub mod extensions;

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use crate::error::GirError;
use crate::hash::ObjectId;
use crate::object::tree::{FileMode, Tree, TreeEntry};
use crate::object::ParsedObject;
use crate::storage::atomic::LockFile;
use crate::storage::ObjectDatabase;

pub use codec::{parse_index_bytes, serialize_index_bytes};
pub use entry::IndexEntry;
pub use extensions::IndexExtension;

/// In-memory representation of `.git/index` (DIRC v2/v3), keyed by `(path, stage)`.
#[derive(Debug, Clone, Default)]
pub struct GitIndex {
    entries: BTreeMap<(String, u8), IndexEntry>,
    extensions: Vec<IndexExtension>,
    index_mtime_sec: Option<u32>,
}

impl GitIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads `.git/index` from `git_dir` (returning an empty `GitIndex` if `.git/index` does not yet exist).
    pub fn read(git_dir: &Path) -> Result<Self, GirError> {
        let index_path = git_dir.join("index");
        if !index_path.is_file() {
            return Ok(Self::new());
        }

        let meta = fs::metadata(&index_path)?;
        let index_mtime_sec = Some((meta.mtime() as u64 & 0xffff_ffff) as u32);
        let bytes = fs::read(&index_path)?;
        let (_, entry_list, extensions) = parse_index_bytes(&bytes)?;

        let mut entries = BTreeMap::new();
        for entry in entry_list {
            entries.insert((entry.path.clone(), entry.stage()), entry);
        }

        Ok(Self {
            entries,
            extensions,
            index_mtime_sec,
        })
    }

    /// Writes `.git/index` atomically using `LockFile` (`.git/index.lock`).
    pub fn write(&self, git_dir: &Path) -> Result<(), GirError> {
        let index_path = git_dir.join("index");
        let mut lock = LockFile::acquire(&index_path)?;

        let ordered: Vec<IndexEntry> = self.entries.values().cloned().collect();
        let bytes = serialize_index_bytes(&ordered, &self.extensions);
        lock.write_all(&bytes)?;
        lock.commit()
    }

    pub fn index_mtime_sec(&self) -> Option<u32> {
        self.index_mtime_sec
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns an iterator over all `IndexEntry` items in canonical `(path, stage)` order.
    pub fn entries(&self) -> impl Iterator<Item = &IndexEntry> {
        self.entries.values()
    }

    /// Looks up an entry by `(path, stage)`.
    pub fn get(&self, path: &str, stage: u8) -> Option<&IndexEntry> {
        self.entries.get(&(path.to_string(), stage))
    }

    /// Returns `true` if `path` is tracked at any stage (`0..=3`) in the index,
    /// or if `path` is a parent directory prefix of any tracked entry.
    pub fn contains_path_or_dir(&self, path: &str) -> bool {
        let clean = path.trim_matches('/');
        if self.entries.contains_key(&(clean.to_string(), 0)) {
            return true;
        }
        let dir_prefix = format!("{clean}/");
        for (p, _) in self.entries.keys() {
            if p == clean || p.starts_with(&dir_prefix) {
                return true;
            }
        }
        false
    }

    /// Returns `true` if the index currently contains any unmerged conflict entries (`stage in 1..=3`).
    pub fn has_conflicts(&self) -> bool {
        self.entries.keys().any(|(_, stage)| *stage != 0)
    }

    /// Inserts or updates a stage-0 `IndexEntry`, clearing any conflict stages (`1..=3`),
    /// removing any conflicting file/directory ancestor or descendant entries,
    /// and invalidating the `TREE` cache-tree extension so subsequent `git commit` reads fresh entries.
    pub fn add_entry(&mut self, entry: IndexEntry) {
        let path = entry.path.clone();
        let stage = entry.stage();
        if stage == 0 {
            for s in 1..=3 {
                self.entries.remove(&(path.clone(), s));
            }
        }

        // Remove any ancestor file entries (e.g. if adding `foo/bar.txt`, remove file `foo`)
        let mut slash_pos = 0;
        while let Some(rel_idx) = path[slash_pos..].find('/') {
            let ancestor = &path[..slash_pos + rel_idx];
            for s in 0..=3 {
                self.entries.remove(&(ancestor.to_string(), s));
            }
            slash_pos += rel_idx + 1;
        }

        // Remove any descendant entries if `path` was previously a directory (`foo/...`)
        let desc_prefix = format!("{path}/");
        let desc_keys: Vec<(String, u8)> = self
            .entries
            .keys()
            .filter(|(p, _)| p.starts_with(&desc_prefix))
            .cloned()
            .collect();
        for k in desc_keys {
            self.entries.remove(&k);
        }

        self.entries.insert((path, stage), entry);
        self.invalidate_cache_tree();
    }

    /// Removes all stages (`0..=3`) for `path` from the index and invalidates the `TREE` extension.
    pub fn remove_path(&mut self, path: &str) -> Option<IndexEntry> {
        let mut removed = None;
        for s in 0..=3 {
            if let Some(e) = self.entries.remove(&(path.to_string(), s)) {
                if s == 0 || removed.is_none() {
                    removed = Some(e);
                }
            }
        }
        if removed.is_some() {
            self.invalidate_cache_tree();
        }
        removed
    }

    /// Removes all entries under `dir_prefix` (or exact match `dir_prefix`) and returns their paths.
    pub fn remove_matching_prefix(&mut self, prefix: &str) -> Vec<String> {
        let clean = prefix.trim_matches('/');
        let slash_prefix = if clean.is_empty() {
            String::new()
        } else {
            format!("{clean}/")
        };

        let keys_to_remove: Vec<(String, u8)> = self
            .entries
            .keys()
            .filter(|(p, _)| clean.is_empty() || p == clean || p.starts_with(&slash_prefix))
            .cloned()
            .collect();

        let mut removed_paths = Vec::new();
        for key in keys_to_remove {
            if self.entries.remove(&key).is_some() {
                removed_paths.push(key.0);
            }
        }
        removed_paths.sort();
        removed_paths.dedup();
        if !removed_paths.is_empty() {
            self.invalidate_cache_tree();
        }
        removed_paths
    }

    /// Replaces the entire index with entries flattened from `tree_oid` in `odb`.
    pub fn populate_from_tree(
        &mut self,
        odb: &ObjectDatabase,
        tree_oid: &ObjectId,
    ) -> Result<(), GirError> {
        self.entries.clear();
        let mut flat = BTreeMap::new();
        flatten_tree_into(odb, tree_oid, "", &mut flat)?;
        for (path, (mode, oid)) in flat {
            let entry = IndexEntry::from_tree_entry(path, mode, oid, 0);
            self.entries.insert((entry.path.clone(), 0), entry);
        }
        self.invalidate_cache_tree();
        Ok(())
    }

    /// Builds a hierarchical Git `Tree` object graph in `odb` from the current stage-0 index entries
    /// and returns the root `Tree`'s `ObjectId`.
    pub fn write_tree(&self, odb: &ObjectDatabase) -> Result<ObjectId, GirError> {
        if self.has_conflicts() {
            return Err(GirError::InvalidArgument(
                "cannot write-tree with unmerged conflict entries in index".to_string(),
            ));
        }
        let stage0: Vec<&IndexEntry> = self
            .entries
            .values()
            .filter(|e| e.stage() == 0)
            .collect();
        write_subtree(odb, &stage0, "")
    }

    /// Drops any cached `TREE` extension so official `/usr/bin/git commit` never uses stale cached tree SHAs.
    pub fn invalidate_cache_tree(&mut self) {
        self.extensions.retain(|ext| !ext.is_cache_tree());
    }
}

/// Flattens a recursive Git `Tree` into a sorted map `rel_path -> (FileMode, ObjectId)`.
pub fn flatten_tree(
    odb: &ObjectDatabase,
    tree_oid: &ObjectId,
) -> Result<BTreeMap<String, (FileMode, ObjectId)>, GirError> {
    let mut out = BTreeMap::new();
    flatten_tree_into(odb, tree_oid, "", &mut out)?;
    Ok(out)
}

fn flatten_tree_into(
    odb: &ObjectDatabase,
    tree_oid: &ObjectId,
    prefix: &str,
    out: &mut BTreeMap<String, (FileMode, ObjectId)>,
) -> Result<(), GirError> {
    let ParsedObject::Tree(tree) = odb.read_parsed(tree_oid)? else {
        return Err(GirError::UnexpectedObjectType {
            expected: "tree".to_string(),
            found: "non-tree".to_string(),
        });
    };

    for entry in tree.entries {
        let rel_path = if prefix.is_empty() {
            entry.name.clone()
        } else {
            format!("{prefix}/{}", entry.name)
        };
        if entry.mode == FileMode::Directory {
            flatten_tree_into(odb, &entry.oid, &rel_path, out)?;
        } else {
            out.insert(rel_path, (entry.mode, entry.oid));
        }
    }
    Ok(())
}

fn write_subtree(
    odb: &ObjectDatabase,
    entries: &[&IndexEntry],
    dir_prefix: &str,
) -> Result<ObjectId, GirError> {
    let mut tree_entries: Vec<TreeEntry> = Vec::new();
    let mut subdirs: BTreeMap<String, Vec<&IndexEntry>> = BTreeMap::new();

    for &entry in entries {
        let rel = if dir_prefix.is_empty() {
            entry.path.as_str()
        } else {
            entry
                .path
                .strip_prefix(dir_prefix)
                .and_then(|s| s.strip_prefix('/'))
                .unwrap_or("")
        };

        if let Some((first_seg, _)) = rel.split_once('/') {
            subdirs
                .entry(first_seg.to_string())
                .or_default()
                .push(entry);
        } else if !rel.is_empty() {
            tree_entries.push(TreeEntry::new(
                entry.file_mode(),
                rel.to_string(),
                entry.oid,
            ));
        }
    }

    for (subdir_name, sub_entries) in subdirs {
        let sub_prefix = if dir_prefix.is_empty() {
            subdir_name.clone()
        } else {
            format!("{dir_prefix}/{subdir_name}")
        };
        let sub_oid = write_subtree(odb, &sub_entries, &sub_prefix)?;
        tree_entries.push(TreeEntry::new(FileMode::Directory, subdir_name, sub_oid));
    }

    let tree = Tree::new(tree_entries);
    odb.write_object(&tree.to_raw())
}
