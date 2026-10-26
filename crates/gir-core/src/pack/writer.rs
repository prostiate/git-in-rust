use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use flate2::write::ZlibEncoder;
use flate2::Compression;
use sha1::{Digest, Sha1};

use crate::error::GirError;
use crate::hash::ObjectId;
use crate::object::{ObjectKind, RawObject};
use crate::pack::delta::{apply_delta, compute_delta};
use crate::pack::index::{write_idx_v2, PackIndexEntry};
use crate::pack::reader::{
    decode_pack_entry_at, validate_pack_slice, verify_pack_slice_checksum, DecodedPackEntry,
    PackEntryKind, PACK_HEADER_LEN, PACK_MAGIC, PACK_TRAILER_LEN, PACK_VERSION,
};
use crate::storage::atomic::write_immutable_atomic;
use crate::storage::ObjectDatabase;

const SLIDING_WINDOW_SIZE: usize = 10;
const MAX_DELTA_DEPTH: usize = 50;

#[derive(Clone)]
struct WindowCandidate {
    offset: u64,
    kind: ObjectKind,
    data: Vec<u8>,
    depth: usize,
}

/// Encodes a slice of `(ObjectId, RawObject)` into an in-memory `.pack` v2 byte buffer,
/// returning `(pack_bytes, index_entries, pack_checksum)`.
pub fn build_pack_bytes(
    objects: &[(ObjectId, RawObject)],
) -> Result<(Vec<u8>, Vec<PackIndexEntry>, ObjectId), GirError> {
    // Group objects by type (Commits -> Trees -> Blobs -> Tags) so delta window compares same-kind objects
    let mut ordered: Vec<(ObjectId, RawObject)> = objects.to_vec();
    ordered.sort_by(|a, b| {
        type_sort_key(a.1.kind)
            .cmp(&type_sort_key(b.1.kind))
            .then_with(|| b.1.data.len().cmp(&a.1.data.len()))
            .then_with(|| a.0.cmp(&b.0))
    });
    ordered.dedup_by(|a, b| a.0 == b.0);

    let count = ordered.len() as u32;
    let mut pack_buf = Vec::new();

    // 1. 12-byte PACK v2 header
    pack_buf.extend_from_slice(PACK_MAGIC);
    pack_buf.extend_from_slice(&PACK_VERSION.to_be_bytes());
    pack_buf.extend_from_slice(&count.to_be_bytes());

    let mut window: VecDeque<WindowCandidate> = VecDeque::with_capacity(SLIDING_WINDOW_SIZE);
    let mut index_entries = Vec::with_capacity(ordered.len());

    // 2. Encode each object (using OBJ_OFS_DELTA when a compact same-type delta exists in window)
    for (oid, raw) in ordered {
        let entry_offset = pack_buf.len() as u64;

        let mut best_delta: Option<(u64, usize, Vec<u8>)> = None;
        for cand in window.iter().rev() {
            if cand.kind != raw.kind || cand.depth >= MAX_DELTA_DEPTH {
                continue;
            }
            if let Some(delta_bytes) = compute_delta(&cand.data, &raw.data) {
                let replace = match &best_delta {
                    None => true,
                    Some((_, _, prev_best)) => delta_bytes.len() < prev_best.len(),
                };
                if replace {
                    best_delta = Some((cand.offset, cand.depth + 1, delta_bytes));
                }
            }
        }

        let depth = if let Some((base_offset, new_depth, delta_bytes)) = best_delta {
            // Write OBJ_OFS_DELTA (type 6)
            encode_entry_header(&mut pack_buf, 6, delta_bytes.len());
            let neg_offset = entry_offset - base_offset;
            encode_ofs_delta_offset(&mut pack_buf, neg_offset);
            compress_zlib_into(&mut pack_buf, &delta_bytes)?;
            new_depth
        } else {
            // Write base object (type 1..=4)
            let raw_type = object_kind_to_pack_type(raw.kind);
            encode_entry_header(&mut pack_buf, raw_type, raw.data.len());
            compress_zlib_into(&mut pack_buf, &raw.data)?;
            0
        };

        let crc32 = crc32fast::hash(&pack_buf[entry_offset as usize..]);
        index_entries.push(PackIndexEntry {
            oid,
            pack_offset: entry_offset,
            crc32,
        });

        if window.len() == SLIDING_WINDOW_SIZE {
            window.pop_front();
        }
        window.push_back(WindowCandidate {
            offset: entry_offset,
            kind: raw.kind,
            data: raw.data,
            depth,
        });
    }

    // 3. Compute and append 20-byte pack SHA-1 trailer
    let mut hasher = Sha1::new();
    hasher.update(&pack_buf);
    let pack_checksum = ObjectId::from_bytes(hasher.finalize().into());
    pack_buf.extend_from_slice(pack_checksum.as_bytes());

    Ok((pack_buf, index_entries, pack_checksum))
}

/// Writes both `pack_path` and `idx_path` atomically from `objects`.
pub fn write_pack_and_index(
    objects: &[(ObjectId, RawObject)],
    pack_path: &Path,
    idx_path: &Path,
) -> Result<(ObjectId, ObjectId), GirError> {
    let (pack_bytes, mut entries, pack_checksum) = build_pack_bytes(objects)?;
    write_immutable_atomic(pack_path, &pack_bytes)?;
    let idx_checksum = write_idx_v2(idx_path, &mut entries, &pack_checksum)?;
    Ok((pack_checksum, idx_checksum))
}

/// Encodes `objects` once and writes `<base_prefix>-<pack_sha>.pack` and `<base_prefix>-<pack_sha>.idx` atomically.
pub fn write_named_pack_and_index(
    objects: &[(ObjectId, RawObject)],
    base_prefix: &Path,
) -> Result<(PathBuf, PathBuf, ObjectId, ObjectId), GirError> {
    let (pack_bytes, mut entries, pack_checksum) = build_pack_bytes(objects)?;
    let base_str = base_prefix.to_string_lossy();
    let pack_path = PathBuf::from(format!("{base_str}-{pack_checksum}.pack"));
    let idx_path = PathBuf::from(format!("{base_str}-{pack_checksum}.idx"));

    if !pack_path.is_file() {
        write_immutable_atomic(&pack_path, &pack_bytes)?;
    }
    let idx_checksum = if !idx_path.is_file() {
        write_idx_v2(&idx_path, &mut entries, &pack_checksum)?
    } else {
        crate::pack::PackIndexV2::open(&idx_path)?.idx_checksum().clone()
    };

    Ok((pack_path, idx_path, pack_checksum, idx_checksum))
}

/// Streams and validates a `.pack` byte slice, resolving all base and delta objects,
/// verifying the pack checksum, and returning `(index_entries, pack_checksum, resolved_objects)`.
pub fn index_pack_bytes(
    path: &Path,
    pack_bytes: &[u8],
    odb: Option<&ObjectDatabase>,
) -> Result<(Vec<PackIndexEntry>, ObjectId, Vec<(ObjectId, RawObject)>), GirError> {
    let (object_count, _) = validate_pack_slice(path, pack_bytes)?;
    let pack_checksum = verify_pack_slice_checksum(path, pack_bytes)?;

    let count = object_count as usize;
    let mut raw_entries = Vec::with_capacity(count);
    let mut cursor = PACK_HEADER_LEN as u64;
    let data_end = (pack_bytes.len() - PACK_TRAILER_LEN) as u64;

    for i in 0..count {
        if cursor >= data_end {
            return Err(GirError::CorruptPack {
                path: path.to_path_buf(),
                reason: format!("unexpected EOF after {i} of {count} objects"),
            });
        }
        let decoded = decode_pack_entry_at(path, pack_bytes, cursor)?;
        cursor = decoded.next_offset;
        raw_entries.push(decoded);
    }

    if cursor != data_end {
        return Err(GirError::CorruptPack {
            path: path.to_path_buf(),
            reason: format!(
                "trailing bytes after last pack entry: stopped at {cursor}, expected {data_end}"
            ),
        });
    }

    // Resolve all entries (including multi-level OFS_DELTA and REF_DELTA chains)
    let mut resolved_by_offset: HashMap<u64, RawObject> = HashMap::with_capacity(count);
    let mut resolved_by_oid: HashMap<ObjectId, RawObject> = HashMap::with_capacity(count);
    let mut pending: Vec<DecodedPackEntry> = Vec::new();

    // First pass: resolve Base and OFS_DELTA entries (since OFS_DELTA always points backward)
    for entry in raw_entries.iter() {
        match entry.kind {
            PackEntryKind::Base(kind) => {
                let raw = RawObject {
                    kind,
                    data: entry.data.clone(),
                };
                let oid = raw.id();
                resolved_by_offset.insert(entry.offset, raw.clone());
                resolved_by_oid.insert(oid, raw);
            }
            PackEntryKind::OfsDelta { base_offset } => {
                if let Some(base) = resolved_by_offset.get(&base_offset) {
                    let reconstructed = apply_delta(&base.data, &entry.data)?;
                    let raw = RawObject {
                        kind: base.kind,
                        data: reconstructed,
                    };
                    let oid = raw.id();
                    resolved_by_offset.insert(entry.offset, raw.clone());
                    resolved_by_oid.insert(oid, raw);
                } else {
                    pending.push(entry.clone());
                }
            }
            PackEntryKind::RefDelta { .. } => {
                pending.push(entry.clone());
            }
        }
    }

    // Iterative pass for REF_DELTA or out-of-order delta chains
    while !pending.is_empty() {
        let prev_len = pending.len();
        let mut next_pending = Vec::new();

        for entry in pending {
            let maybe_base = match entry.kind {
                PackEntryKind::Base(_) => unreachable!(),
                PackEntryKind::OfsDelta { base_offset } => {
                    resolved_by_offset.get(&base_offset).cloned()
                }
                PackEntryKind::RefDelta { base_oid } => resolved_by_oid
                    .get(&base_oid)
                    .cloned()
                    .or_else(|| odb.and_then(|db| db.read_raw(&base_oid).ok())),
            };

            if let Some(base) = maybe_base {
                let reconstructed = apply_delta(&base.data, &entry.data)?;
                let raw = RawObject {
                    kind: base.kind,
                    data: reconstructed,
                };
                let oid = raw.id();
                resolved_by_offset.insert(entry.offset, raw.clone());
                resolved_by_oid.insert(oid, raw);
            } else {
                next_pending.push(entry);
            }
        }

        if next_pending.len() == prev_len {
            return Err(GirError::CorruptPack {
                path: path.to_path_buf(),
                reason: format!("unable to resolve {} delta entries in pack", next_pending.len()),
            });
        }
        pending = next_pending;
    }

    let mut index_entries = Vec::with_capacity(count);
    let mut resolved_objects = Vec::with_capacity(count);

    for entry in raw_entries {
        let raw = resolved_by_offset
            .remove(&entry.offset)
            .expect("all entries resolved");
        let oid = raw.id();
        index_entries.push(PackIndexEntry {
            oid,
            pack_offset: entry.offset,
            crc32: entry.crc32,
        });
        resolved_objects.push((oid, raw));
    }

    Ok((index_entries, pack_checksum, resolved_objects))
}

/// Streams an existing `.pack` file on disk, verifies all objects and checksums, and writes its `.idx` v2 file.
pub fn index_pack_file(
    pack_path: &Path,
    idx_path: Option<&Path>,
    odb: Option<&ObjectDatabase>,
) -> Result<(PathBuf, ObjectId, ObjectId), GirError> {
    let pack_bytes = fs::read(pack_path)?;
    let (mut entries, pack_checksum, _) = index_pack_bytes(pack_path, &pack_bytes, odb)?;

    let out_idx_path = match idx_path {
        Some(p) => p.to_path_buf(),
        None => pack_path.with_extension("idx"),
    };

    let idx_checksum = write_idx_v2(&out_idx_path, &mut entries, &pack_checksum)?;
    Ok((out_idx_path, pack_checksum, idx_checksum))
}

/// Unpacks all objects from a `.pack` byte slice directly into the loose object store (`odb`).
pub fn unpack_pack_bytes(
    pack_bytes: &[u8],
    odb: &ObjectDatabase,
) -> Result<Vec<ObjectId>, GirError> {
    let (_, _, resolved) = index_pack_bytes(Path::new("<pack-stream>"), pack_bytes, Some(odb))?;
    let mut ids = Vec::with_capacity(resolved.len());
    for (oid, raw) in resolved {
        odb.write_object(&raw)?;
        ids.push(oid);
    }
    Ok(ids)
}

fn type_sort_key(kind: ObjectKind) -> u8 {
    match kind {
        ObjectKind::Commit => 1,
        ObjectKind::Tree => 2,
        ObjectKind::Blob => 3,
        ObjectKind::Tag => 4,
    }
}

fn object_kind_to_pack_type(kind: ObjectKind) -> u8 {
    match kind {
        ObjectKind::Commit => 1,
        ObjectKind::Tree => 2,
        ObjectKind::Blob => 3,
        ObjectKind::Tag => 4,
    }
}

fn encode_entry_header(out: &mut Vec<u8>, raw_type: u8, mut size: usize) {
    let mut b0 = ((raw_type & 0x07) << 4) | ((size & 0x0f) as u8);
    size >>= 4;
    if size != 0 {
        b0 |= 0x80;
    }
    out.push(b0);

    while size != 0 {
        let mut b = (size & 0x7f) as u8;
        size >>= 7;
        if size != 0 {
            b |= 0x80;
        }
        out.push(b);
    }
}

fn encode_ofs_delta_offset(out: &mut Vec<u8>, mut neg_offset: u64) {
    let mut buf = [0u8; 10];
    let mut pos = buf.len() - 1;
    buf[pos] = (neg_offset & 0x7f) as u8;
    neg_offset >>= 7;

    while neg_offset > 0 {
        neg_offset -= 1;
        pos -= 1;
        buf[pos] = 0x80 | ((neg_offset & 0x7f) as u8);
        neg_offset >>= 7;
    }

    out.extend_from_slice(&buf[pos..]);
}

fn compress_zlib_into(out: &mut Vec<u8>, data: &[u8]) -> Result<(), GirError> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data)?;
    let compressed = encoder.finish()?;
    out.extend_from_slice(&compressed);
    Ok(())
}
