use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use flate2::{Decompress, FlushDecompress, Status};
use memmap2::Mmap;
use sha1::{Digest, Sha1};

use crate::error::GirError;
use crate::hash::ObjectId;
use crate::object::{ObjectKind, RawObject};
use crate::pack::delta::apply_delta;
use crate::storage::ObjectDatabase;

pub const PACK_MAGIC: &[u8; 4] = b"PACK";
pub const PACK_VERSION: u32 = 2;
pub const PACK_HEADER_LEN: usize = 12;
pub const PACK_TRAILER_LEN: usize = 20;

const MAX_BASE_CACHE_ENTRIES: usize = 256;

/// Classification of an entry header inside a `.pack` stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackEntryKind {
    Base(ObjectKind),
    OfsDelta { base_offset: u64 },
    RefDelta { base_oid: ObjectId },
}

/// Decoded single entry from a `.pack` byte slice prior to delta application.
#[derive(Debug, Clone)]
pub struct DecodedPackEntry {
    pub offset: u64,
    pub next_offset: u64,
    pub crc32: u32,
    pub kind: PackEntryKind,
    pub data: Vec<u8>,
}

/// Memory-mapped reader for a Git `.pack` v2 file with a thread-safe base-object cache.
#[derive(Debug, Clone)]
pub struct PackFile {
    path: PathBuf,
    mmap: Arc<Mmap>,
    object_count: u32,
    checksum: ObjectId,
    base_cache: Arc<Mutex<HashMap<u64, RawObject>>>,
}

impl PackFile {
    /// Opens a `.pack` v2 file using OS memory mapping and validates its 12-byte header.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, GirError> {
        let path = path.as_ref().to_path_buf();
        let file = File::open(&path)?;
        // SAFETY: Git packfiles are write-once immutable artifacts.
        let mmap = unsafe { Mmap::map(&file)? };

        let (object_count, checksum) = validate_pack_slice(&path, &mmap)?;

        Ok(Self {
            path,
            mmap: Arc::new(mmap),
            object_count,
            checksum,
            base_cache: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn object_count(&self) -> u32 {
        self.object_count
    }

    pub fn checksum(&self) -> &ObjectId {
        &self.checksum
    }

    /// Verifies the trailing 20-byte SHA-1 checksum over the entire `.pack` content.
    pub fn verify_checksum(&self) -> Result<(), GirError> {
        verify_pack_slice_checksum(&self.path, &self.mmap)?;
        Ok(())
    }

    /// Reads and reconstructs the `RawObject` at `offset` in this packfile, resolving any
    /// `OBJ_OFS_DELTA` or `OBJ_REF_DELTA` chains.
    pub fn read_object_at(
        &self,
        offset: u64,
        odb: Option<&ObjectDatabase>,
    ) -> Result<RawObject, GirError> {
        if let Ok(cache) = self.base_cache.lock() {
            if let Some(cached) = cache.get(&offset) {
                return Ok(cached.clone());
            }
        }

        let entry = decode_pack_entry_at(&self.path, &self.mmap, offset)?;
        let resolved = match entry.kind {
            PackEntryKind::Base(kind) => RawObject {
                kind,
                data: entry.data,
            },
            PackEntryKind::OfsDelta { base_offset } => {
                let base = self.read_object_at(base_offset, odb)?;
                let reconstructed = apply_delta(&base.data, &entry.data)?;
                RawObject {
                    kind: base.kind,
                    data: reconstructed,
                }
            }
            PackEntryKind::RefDelta { base_oid } => {
                let Some(db) = odb else {
                    return Err(GirError::CorruptPack {
                        path: self.path.clone(),
                        reason: format!(
                            "cannot resolve OBJ_REF_DELTA base {base_oid} without an ObjectDatabase"
                        ),
                    });
                };
                let base = db.read_raw(&base_oid)?;
                let reconstructed = apply_delta(&base.data, &entry.data)?;
                RawObject {
                    kind: base.kind,
                    data: reconstructed,
                }
            }
        };

        if let Ok(mut cache) = self.base_cache.lock() {
            if cache.len() >= MAX_BASE_CACHE_ENTRIES {
                cache.clear();
            }
            cache.insert(offset, resolved.clone());
        }

        Ok(resolved)
    }

    /// Computes the on-disk CRC32 of the raw packed entry at `offset`.
    pub fn entry_crc32_at(&self, offset: u64) -> Result<u32, GirError> {
        let entry = decode_pack_entry_at(&self.path, &self.mmap, offset)?;
        Ok(entry.crc32)
    }
}

/// Validates the 12-byte `PACK` v2 header and extracts `(object_count, pack_checksum)` from a raw pack slice.
pub fn validate_pack_slice(path: &Path, bytes: &[u8]) -> Result<(u32, ObjectId), GirError> {
    if bytes.len() < PACK_HEADER_LEN + PACK_TRAILER_LEN {
        return Err(GirError::CorruptPack {
            path: path.to_path_buf(),
            reason: format!("packfile too short ({} bytes)", bytes.len()),
        });
    }

    if &bytes[0..4] != PACK_MAGIC {
        return Err(GirError::CorruptPack {
            path: path.to_path_buf(),
            reason: "invalid packfile magic (expected 'PACK')".to_string(),
        });
    }

    let version = u32::from_be_bytes(bytes[4..8].try_into().unwrap());
    if version != PACK_VERSION {
        return Err(GirError::CorruptPack {
            path: path.to_path_buf(),
            reason: format!("unsupported packfile version {version} (expected 2)"),
        });
    }

    let object_count = u32::from_be_bytes(bytes[8..12].try_into().unwrap());
    let trailer_start = bytes.len() - PACK_TRAILER_LEN;
    let checksum = ObjectId::from_slice(&bytes[trailer_start..]).unwrap();

    Ok((object_count, checksum))
}

/// Verifies the trailing 20-byte SHA-1 checksum of a `.pack` byte slice.
pub fn verify_pack_slice_checksum(path: &Path, bytes: &[u8]) -> Result<ObjectId, GirError> {
    let (_, expected) = validate_pack_slice(path, bytes)?;
    let data_len = bytes.len() - PACK_TRAILER_LEN;

    let mut hasher = Sha1::new();
    hasher.update(&bytes[..data_len]);
    let actual = ObjectId::from_bytes(hasher.finalize().into());

    if actual != expected {
        return Err(GirError::CorruptPack {
            path: path.to_path_buf(),
            reason: format!("packfile SHA-1 mismatch: expected {expected}, computed {actual}"),
        });
    }
    Ok(actual)
}

/// Decodes a single raw entry at `entry_offset` inside `pack_bytes`, inflating its zlib payload
/// and computing the exact byte boundary (`next_offset`) and CRC32 of the raw packed entry.
pub fn decode_pack_entry_at(
    path: &Path,
    pack_bytes: &[u8],
    entry_offset: u64,
) -> Result<DecodedPackEntry, GirError> {
    let data_end = pack_bytes.len().saturating_sub(PACK_TRAILER_LEN);
    let start = entry_offset as usize;
    if start < PACK_HEADER_LEN || start >= data_end {
        return Err(GirError::CorruptPack {
            path: path.to_path_buf(),
            reason: format!("pack entry offset {entry_offset} out of bounds (data_end={data_end})"),
        });
    }

    let mut pos = start;

    // 1. Parse variable-length object header (type + uncompressed size)
    let b0 = pack_bytes[pos];
    pos += 1;
    let raw_type = (b0 >> 4) & 0x07;
    let mut uncompressed_size = (b0 & 0x0f) as usize;
    let mut shift = 4u32;
    let mut prev = b0;

    while (prev & 0x80) != 0 {
        if pos >= data_end {
            return Err(GirError::CorruptPack {
                path: path.to_path_buf(),
                reason: "truncated entry header varint".to_string(),
            });
        }
        let b = pack_bytes[pos];
        pos += 1;
        uncompressed_size |= ((b & 0x7f) as usize) << shift;
        shift += 7;
        prev = b;
    }

    // 2. Parse entry kind and delta base specifier (if delta)
    let kind = match raw_type {
        1 => PackEntryKind::Base(ObjectKind::Commit),
        2 => PackEntryKind::Base(ObjectKind::Tree),
        3 => PackEntryKind::Base(ObjectKind::Blob),
        4 => PackEntryKind::Base(ObjectKind::Tag),
        6 => {
            // OBJ_OFS_DELTA: biased variable-length negative offset from `entry_offset`
            if pos >= data_end {
                return Err(GirError::CorruptPack {
                    path: path.to_path_buf(),
                    reason: "truncated OBJ_OFS_DELTA offset".to_string(),
                });
            }
            let mut b = pack_bytes[pos];
            pos += 1;
            let mut neg_offset = (b & 0x7f) as u64;
            while (b & 0x80) != 0 {
                if pos >= data_end {
                    return Err(GirError::CorruptPack {
                        path: path.to_path_buf(),
                        reason: "truncated OBJ_OFS_DELTA continuation byte".to_string(),
                    });
                }
                neg_offset += 1;
                b = pack_bytes[pos];
                pos += 1;
                neg_offset = (neg_offset << 7) | ((b & 0x7f) as u64);
            }
            let Some(base_offset) = entry_offset.checked_sub(neg_offset) else {
                return Err(GirError::CorruptPack {
                    path: path.to_path_buf(),
                    reason: format!(
                        "invalid OBJ_OFS_DELTA negative offset {neg_offset} at {entry_offset}"
                    ),
                });
            };
            PackEntryKind::OfsDelta { base_offset }
        }
        7 => {
            // OBJ_REF_DELTA: 20-byte base ObjectId
            if pos + 20 > data_end {
                return Err(GirError::CorruptPack {
                    path: path.to_path_buf(),
                    reason: "truncated OBJ_REF_DELTA base OID".to_string(),
                });
            }
            let base_oid = ObjectId::from_slice(&pack_bytes[pos..pos + 20]).unwrap();
            pos += 20;
            PackEntryKind::RefDelta { base_oid }
        }
        other => {
            return Err(GirError::CorruptPack {
                path: path.to_path_buf(),
                reason: format!("invalid pack entry type id {other} at offset {entry_offset}"),
            });
        }
    };

    // 3. Inflate exact single zlib stream starting at `pos`
    let zlib_slice = &pack_bytes[pos..data_end];
    let mut decompressor = Decompress::new(true);
    // Reserve +64 bytes so 0-length payloads still provide buffer capacity to consume zlib headers/trailers
    let mut inflated = Vec::with_capacity(uncompressed_size + 64);

    loop {
        let consumed = decompressor.total_in() as usize;
        if inflated.len() == inflated.capacity() {
            inflated.reserve(4096);
        }
        let status = decompressor
            .decompress_vec(
                &zlib_slice[consumed..],
                &mut inflated,
                FlushDecompress::None,
            )
            .map_err(|e| GirError::CorruptPack {
                path: path.to_path_buf(),
                reason: format!("zlib error at pack offset {entry_offset}: {e}"),
            })?;

        match status {
            Status::StreamEnd => break,
            Status::Ok => {
                if decompressor.total_in() as usize == consumed && inflated.len() < inflated.capacity() {
                    return Err(GirError::CorruptPack {
                        path: path.to_path_buf(),
                        reason: format!("unexpected EOF in zlib stream at offset {entry_offset}"),
                    });
                }
            }
            Status::BufError => {
                inflated.reserve(4096);
            }
        }
    }

    if inflated.len() != uncompressed_size {
        return Err(GirError::CorruptPack {
            path: path.to_path_buf(),
            reason: format!(
                "pack entry size mismatch at offset {entry_offset}: header declared {uncompressed_size}, inflated {}",
                inflated.len()
            ),
        });
    }

    let compressed_len = decompressor.total_in() as usize;
    let end = pos + compressed_len;
    let crc32 = crc32fast::hash(&pack_bytes[start..end]);

    Ok(DecodedPackEntry {
        offset: entry_offset,
        next_offset: end as u64,
        crc32,
        kind,
        data: inflated,
    })
}
