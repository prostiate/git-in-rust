use std::collections::{HashSet, VecDeque};
use std::fs;
use std::io::Write;

use crate::error::GirError;
use crate::hash::ObjectId;
use crate::object::{ObjectKind, ParsedObject};
use crate::pack::write_named_pack_and_index;
use crate::repository::Repository;
use crate::storage::atomic::LockFile;

/// Summary report returned by `fsck_repository`.
#[derive(Debug, Clone, Default)]
pub struct FsckReport {
    pub checked_objects: usize,
    pub checked_packs: usize,
    pub dangling_objects: Vec<(ObjectKind, ObjectId)>,
    pub errors: Vec<String>,
}

impl FsckReport {
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Summary report returned by `gc_repository`.
#[derive(Debug, Clone)]
pub struct GcReport {
    pub packed_objects: usize,
    pub removed_loose_objects: usize,
    pub pack_checksum: Option<ObjectId>,
}

/// Performs a full cryptographic and structural integrity check of the repository's object database.
pub fn fsck_repository(repo: &Repository, full: bool) -> Result<FsckReport, GirError> {
    let mut report = FsckReport::default();
    let odb = repo.odb();

    // 1. Verify all packfiles and .idx v2 indices
    let bundles = odb.refresh_packs();
    for bundle in &bundles {
        if let Err(e) = bundle.index.verify_checksum() {
            report.errors.push(format!("index checksum error: {e}"));
        }
        if full {
            if let Err(e) = bundle.pack.verify_checksum() {
                report.errors.push(format!("pack checksum error: {e}"));
            }
            if let Ok(entries) = bundle.index.entries() {
                for entry in entries {
                    match bundle.pack.entry_crc32_at(entry.pack_offset) {
                        Ok(actual_crc) if actual_crc != entry.crc32 => {
                            report.errors.push(format!(
                                "CRC32 mismatch for {} in {}: expected {:#010x}, got {:#010x}",
                                entry.oid,
                                bundle.pack.path().display(),
                                entry.crc32,
                                actual_crc
                            ));
                        }
                        Err(e) => {
                            report.errors.push(format!(
                                "corrupt pack entry for {} at offset {}: {e}",
                                entry.oid, entry.pack_offset
                            ));
                        }
                        _ => {}
                    }
                }
            }
        }
        report.checked_packs += 1;
    }

    // 2. Verify every object and its outgoing DAG links
    let all_oids = odb.list_all_objects()?;
    let all_set: HashSet<ObjectId> = all_oids.iter().copied().collect();
    let mut parsed_kinds = Vec::with_capacity(all_oids.len());

    for oid in &all_oids {
        match odb.read_parsed(oid) {
            Ok(parsed) => {
                parsed_kinds.push((*oid, parsed.kind()));
                match parsed {
                    ParsedObject::Blob(_) => {}
                    ParsedObject::Tree(tree) => {
                        for entry in &tree.entries {
                            if !all_set.contains(&entry.oid) {
                                report.errors.push(format!(
                                    "broken link from tree {oid} to missing entry {} ({})",
                                    entry.oid, entry.name
                                ));
                            }
                        }
                    }
                    ParsedObject::Commit(commit) => {
                        if !all_set.contains(&commit.tree) {
                            report.errors.push(format!(
                                "broken link from commit {oid} to missing tree {}",
                                commit.tree
                            ));
                        }
                        for parent in &commit.parents {
                            if !all_set.contains(parent) {
                                report.errors.push(format!(
                                    "broken link from commit {oid} to missing parent {parent}"
                                ));
                            }
                        }
                    }
                    ParsedObject::Tag(tag) => {
                        if !all_set.contains(&tag.object) {
                            report.errors.push(format!(
                                "broken link from tag {oid} to missing target {}",
                                tag.object
                            ));
                        }
                    }
                }
                report.checked_objects += 1;
            }
            Err(e) => {
                report.errors.push(format!("corrupt object {oid}: {e}"));
            }
        }
    }

    // 3. Walk reachability from all refs to identify dangling objects
    let reachable = compute_reachable_objects(repo)?;
    if !reachable.is_empty() {
        for (oid, kind) in parsed_kinds {
            if !reachable.contains(&oid) {
                report.dangling_objects.push((kind, oid));
            }
        }
    }

    Ok(report)
}

/// Consolidates objects into a single `.pack` + `.idx` v2 bundle inside `.git/objects/pack/`,
/// packs loose refs into `.git/packed-refs`, and cleans up packed loose objects.
pub fn gc_repository(repo: &Repository, prune: bool) -> Result<GcReport, GirError> {
    let odb = repo.odb();
    let loose_oids = odb.loose().list_all()?;

    let target_oids: Vec<ObjectId> = if prune {
        let reachable = compute_reachable_objects(repo)?;
        if reachable.is_empty() {
            odb.list_all_objects()?
        } else {
            let mut v: Vec<ObjectId> = reachable.into_iter().collect();
            v.sort();
            v
        }
    } else {
        odb.list_all_objects()?
    };

    if target_oids.is_empty() {
        return Ok(GcReport {
            packed_objects: 0,
            removed_loose_objects: 0,
            pack_checksum: None,
        });
    }

    let mut objects_to_pack = Vec::with_capacity(target_oids.len());
    for oid in &target_oids {
        let raw = odb.read_raw(oid)?;
        objects_to_pack.push((*oid, raw));
    }

    let pack_dir = odb.pack_dir();
    fs::create_dir_all(&pack_dir)?;

    let old_bundles = odb.packs();

    // Encode once and write `pack-<sha>.{pack,idx}` atomically
    let base_prefix = pack_dir.join("pack");
    let (pack_path, idx_path, pack_checksum, _) =
        write_named_pack_and_index(&objects_to_pack, &base_prefix)?;

    // Refresh packs so `odb` immediately sees the new packfile
    odb.refresh_packs();

    // Remove older superseded packfiles
    for old in old_bundles {
        if old.pack.path() != pack_path {
            remove_readonly_file(old.pack.path());
        }
        if old.index.path() != idx_path {
            remove_readonly_file(old.index.path());
        }
    }
    odb.refresh_packs();

    // Remove loose objects that are now safely stored in the new pack (or pruned)
    let packed_set: HashSet<ObjectId> = target_oids.iter().copied().collect();
    let mut removed_loose = 0;
    for loose_oid in &loose_oids {
        if packed_set.contains(loose_oid) || prune {
            odb.loose().remove(loose_oid)?;
            removed_loose += 1;
        }
    }

    // Pack loose refs into `.git/packed-refs`
    pack_loose_refs(repo)?;

    Ok(GcReport {
        packed_objects: target_oids.len(),
        removed_loose_objects: removed_loose,
        pack_checksum: Some(pack_checksum),
    })
}

/// Traverses the commit/tree/tag object graph starting from all repository references.
pub fn compute_reachable_objects(repo: &Repository) -> Result<HashSet<ObjectId>, GirError> {
    let odb = repo.odb();
    let refs = repo.list_all_ref_targets()?;
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();

    for (_, oid) in refs {
        if visited.insert(oid) {
            queue.push_back(oid);
        }
    }

    while let Some(oid) = queue.pop_front() {
        let Ok(parsed) = odb.read_parsed(&oid) else {
            continue;
        };
        match parsed {
            ParsedObject::Blob(_) => {}
            ParsedObject::Tree(tree) => {
                for entry in tree.entries {
                    if visited.insert(entry.oid) {
                        queue.push_back(entry.oid);
                    }
                }
            }
            ParsedObject::Commit(commit) => {
                if visited.insert(commit.tree) {
                    queue.push_back(commit.tree);
                }
                for parent in commit.parents {
                    if visited.insert(parent) {
                        queue.push_back(parent);
                    }
                }
            }
            ParsedObject::Tag(tag) => {
                if visited.insert(tag.object) {
                    queue.push_back(tag.object);
                }
            }
        }
    }

    Ok(visited)
}

fn pack_loose_refs(repo: &Repository) -> Result<(), GirError> {
    let refs = repo.list_all_ref_targets()?;
    let packed_refs_vals: Vec<&(String, ObjectId)> = refs
        .iter()
        .filter(|(name, _)| name.starts_with("refs/"))
        .collect();

    if packed_refs_vals.is_empty() {
        return Ok(());
    }

    let packed_refs_path = repo.git_dir().join("packed-refs");
    let mut lock = LockFile::acquire(&packed_refs_path)?;
    writeln!(lock, "# pack-refs with: peeled fully-peeled sorted")?;
    for (ref_name, oid) in &packed_refs_vals {
        writeln!(lock, "{oid} {ref_name}")?;
        // Peel annotated tag chains recursively until reaching the target non-tag object
        let mut current = *oid;
        let mut peeled = None;
        for _ in 0..10 {
            if let Ok(ParsedObject::Tag(tag)) = repo.odb().read_parsed(&current) {
                peeled = Some(tag.object);
                current = tag.object;
            } else {
                break;
            }
        }
        if let Some(target_oid) = peeled {
            writeln!(lock, "^{target_oid}")?;
        }
    }
    lock.commit()?;

    // Remove non-symbolic loose ref files that are now safely stored in `.git/packed-refs`
    for (ref_name, _) in &packed_refs_vals {
        let loose_path = repo.git_dir().join(ref_name);
        if loose_path.is_file() {
            if let Ok(content) = fs::read_to_string(&loose_path) {
                if !content.trim_start().starts_with("ref:") {
                    let _ = fs::remove_file(&loose_path);
                }
            }
        }
    }

    Ok(())
}

fn remove_readonly_file(path: &std::path::Path) {
    if path.is_file() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = fs::metadata(path) {
                let mut perms = meta.permissions();
                perms.set_mode(0o644);
                let _ = fs::set_permissions(path, perms);
            }
        }
        let _ = fs::remove_file(path);
    }
}
