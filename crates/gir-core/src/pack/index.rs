use std::cmp::Ordering;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use memmap2::Mmap;
use sha1::{Digest, Sha1};

use crate::error::GirError;
use crate::hash::{validate_hex_prefix, ObjectId};
use crate::storage::atomic::write_immutable_atomic;

const IDX_V2_MAGIC: [u8; 4] = [0xff, 0x74, 0x4f, 0x63]; // "\xfftOc"
const IDX_V2_VERSION: u32 = 2;
const HEADER_LEN: usize = 8;
const FANOUT_ENTRIES: usize = 256;
const FANOUT_LEN: usize = FANOUT_ENTRIES * 4; // 1024 bytes
const SHA_TABLE_START: usize = HEADER_LEN + FANOUT_LEN; // 1032
const TRAILER_LEN: usize = 40; // 20-byte pack SHA-1 + 20-byte idx SHA-1

/// Metadata for a single packed object entry in a `.idx` v2 file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackIndexEntry {
    pub oid: ObjectId,
    pub pack_offset: u64,
    pub crc32: u32,
}

/// Memory-mapped reader for Git Packfile Index Version 2 (`.idx`).
#[derive(Debug, Clone)]
pub struct PackIndexV2 {
    path: PathBuf,
    mmap: Arc<Mmap>,
    object_count: u32,
    pack_checksum: ObjectId,
    idx_checksum: ObjectId,
}

impl PackIndexV2 {
    /// Opens and validates a `.idx` v2 file using OS memory mapping (`mmap`).
    pub fn open(path: impl AsRef<Path>) -> Result<Self, GirError> {
        let path = path.as_ref().to_path_buf();
        let file = File::open(&path)?;
        // SAFETY: Pack index files in Git are write-once, immutable files written via atomic rename.
        let mmap = unsafe { Mmap::map(&file)? };

        if mmap.len() < SHA_TABLE_START + TRAILER_LEN {
            return Err(GirError::CorruptPack {
                path,
                reason: format!("index file too short ({} bytes)", mmap.len()),
            });
        }

        if mmap[0..4] != IDX_V2_MAGIC {
            return Err(GirError::CorruptPack {
                path,
                reason: "invalid .idx magic header (expected \\xfftOc)".to_string(),
            });
        }

        let version = u32::from_be_bytes(mmap[4..8].try_into().unwrap());
        if version != IDX_V2_VERSION {
            return Err(GirError::CorruptPack {
                path,
                reason: format!("unsupported .idx version {version} (expected 2)"),
            });
        }

        // Validate monotonic fanout table
        let mut prev_count = 0u32;
        for i in 0..FANOUT_ENTRIES {
            let offset = HEADER_LEN + i * 4;
            let count = u32::from_be_bytes(mmap[offset..offset + 4].try_into().unwrap());
            if count < prev_count {
                return Err(GirError::CorruptPack {
                    path,
                    reason: format!("non-monotonic fanout table at index {i}"),
                });
            }
            prev_count = count;
        }

        let object_count = prev_count;
        let n = object_count as usize;
        let min_expected_len = SHA_TABLE_START + n * (20 + 4 + 4) + TRAILER_LEN;
        if mmap.len() < min_expected_len {
            return Err(GirError::CorruptPack {
                path,
                reason: format!(
                    "truncated .idx file: expected at least {min_expected_len} bytes for {n} objects, got {}",
                    mmap.len()
                ),
            });
        }

        let trailer_start = mmap.len() - TRAILER_LEN;
        let pack_checksum = ObjectId::from_slice(&mmap[trailer_start..trailer_start + 20]).unwrap();
        let idx_checksum = ObjectId::from_slice(&mmap[trailer_start + 20..]).unwrap();

        Ok(Self {
            path,
            mmap: Arc::new(mmap),
            object_count,
            pack_checksum,
            idx_checksum,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn object_count(&self) -> u32 {
        self.object_count
    }

    pub fn pack_checksum(&self) -> &ObjectId {
        &self.pack_checksum
    }

    pub fn idx_checksum(&self) -> &ObjectId {
        &self.idx_checksum
    }

    /// Verifies the trailing 20-byte SHA-1 checksum of this `.idx` file.
    pub fn verify_checksum(&self) -> Result<(), GirError> {
        let content_len = self.mmap.len() - 20;
        let mut hasher = Sha1::new();
        hasher.update(&self.mmap[..content_len]);
        let digest: [u8; 20] = hasher.finalize().into();
        let computed = ObjectId::from_bytes(digest);
        if computed != self.idx_checksum {
            return Err(GirError::CorruptPack {
                path: self.path.clone(),
                reason: format!(
                    "index SHA-1 mismatch: expected {}, computed {}",
                    self.idx_checksum, computed
                ),
            });
        }
        Ok(())
    }

    /// Looks up an `ObjectId` in $O(\log(N / 256))$ time using the first-level fanout table
    /// followed by binary search over the contiguous 20-byte SHA table.
    pub fn lookup(&self, id: &ObjectId) -> Result<Option<PackIndexEntry>, GirError> {
        let first_byte = id.as_bytes()[0] as usize;
        let (lo, hi) = self.fanout_range(first_byte);
        if lo >= hi {
            return Ok(None);
        }

        let target = id.as_bytes();
        let mut left = lo;
        let mut right = hi;
        while left < right {
            let mid = left + (right - left) / 2;
            let mid_sha = self.sha_at(mid);
            match mid_sha.cmp(target) {
                Ordering::Less => left = mid + 1,
                Ordering::Greater => right = mid,
                Ordering::Equal => {
                    return Ok(Some(self.entry_at(mid)?));
                }
            }
        }

        Ok(None)
    }

    /// Resolves a 4..=40 character hex prefix against the sorted SHA table in this `.idx`.
    pub fn resolve_prefix(&self, hex_prefix: &str) -> Result<Vec<ObjectId>, GirError> {
        let prefix = validate_hex_prefix(hex_prefix)?;
        if prefix.len() == ObjectId::HEX_LEN {
            let oid = ObjectId::from_hex(&prefix)?;
            return Ok(if self.lookup(&oid)?.is_some() {
                vec![oid]
            } else {
                Vec::new()
            });
        }

        let first_byte = u8::from_str_radix(&prefix[0..2], 16).map_err(|_| {
            GirError::InvalidObjectPrefix(hex_prefix.to_string())
        })? as usize;

        let (lo, hi) = self.fanout_range(first_byte);
        let mut matches = Vec::new();
        for idx in lo..hi {
            let oid = ObjectId::from_slice(self.sha_at(idx)).unwrap();
            let hex = oid.to_hex();
            if hex.starts_with(&prefix) {
                matches.push(oid);
            } else if hex.as_str() > prefix.as_str() && !matches.is_empty() {
                // Since the SHA table is strictly sorted, once we pass the prefix range we can stop
                break;
            }
        }

        Ok(matches)
    }

    /// Returns all entries in index order (sorted by `ObjectId`).
    pub fn entries(&self) -> Result<Vec<PackIndexEntry>, GirError> {
        let n = self.object_count as usize;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            out.push(self.entry_at(i)?);
        }
        Ok(out)
    }

    fn fanout_range(&self, first_byte: usize) -> (usize, usize) {
        let lo = if first_byte == 0 {
            0
        } else {
            self.fanout_at(first_byte - 1) as usize
        };
        let hi = self.fanout_at(first_byte) as usize;
        (lo, hi)
    }

    #[inline]
    fn fanout_at(&self, byte_idx: usize) -> u32 {
        let offset = HEADER_LEN + byte_idx * 4;
        u32::from_be_bytes(self.mmap[offset..offset + 4].try_into().unwrap())
    }

    #[inline]
    fn sha_at(&self, index: usize) -> &[u8] {
        let offset = SHA_TABLE_START + index * 20;
        &self.mmap[offset..offset + 20]
    }

    fn entry_at(&self, index: usize) -> Result<PackIndexEntry, GirError> {
        let n = self.object_count as usize;
        let oid = ObjectId::from_slice(self.sha_at(index)).unwrap();

        let crc_table_start = SHA_TABLE_START + n * 20;
        let crc_offset = crc_table_start + index * 4;
        let crc32 = u32::from_be_bytes(self.mmap[crc_offset..crc_offset + 4].try_into().unwrap());

        let offset32_table_start = crc_table_start + n * 4;
        let off32_pos = offset32_table_start + index * 4;
        let raw_off32 = u32::from_be_bytes(self.mmap[off32_pos..off32_pos + 4].try_into().unwrap());

        let pack_offset = if (raw_off32 & 0x8000_0000) == 0 {
            raw_off32 as u64
        } else {
            let ext_idx = (raw_off32 & 0x7fff_ffff) as usize;
            let offset64_table_start = offset32_table_start + n * 4;
            let off64_pos = offset64_table_start + ext_idx * 8;
            let trailer_start = self.mmap.len() - TRAILER_LEN;
            if off64_pos + 8 > trailer_start {
                return Err(GirError::CorruptPack {
                    path: self.path.clone(),
                    reason: format!("extended 64-bit offset index {ext_idx} out of bounds"),
                });
            }
            u64::from_be_bytes(self.mmap[off64_pos..off64_pos + 8].try_into().unwrap())
        };

        Ok(PackIndexEntry {
            oid,
            pack_offset,
            crc32,
        })
    }
}

/// Serializes and atomically writes a Git `.idx` version 2 file from a slice of `PackIndexEntry` items.
pub fn write_idx_v2(
    idx_path: &Path,
    entries: &mut [PackIndexEntry],
    pack_checksum: &ObjectId,
) -> Result<ObjectId, GirError> {
    entries.sort_by(|a, b| a.oid.cmp(&b.oid));

    let n = entries.len();
    let mut buf = Vec::with_capacity(SHA_TABLE_START + n * 28 + TRAILER_LEN);

    // 1. 8-byte header
    buf.extend_from_slice(&IDX_V2_MAGIC);
    buf.extend_from_slice(&IDX_V2_VERSION.to_be_bytes());

    // 2. 256-entry fanout table
    let mut counts = [0u32; FANOUT_ENTRIES];
    for entry in entries.iter() {
        let first = entry.oid.as_bytes()[0] as usize;
        counts[first] += 1;
    }
    let mut cumulative = 0u32;
    for count in &counts {
        cumulative += *count;
        buf.extend_from_slice(&cumulative.to_be_bytes());
    }

    // 3. Sorted 20-byte SHA-1 table
    for entry in entries.iter() {
        buf.extend_from_slice(entry.oid.as_bytes());
    }

    // 4. 4-byte CRC32 table
    for entry in entries.iter() {
        buf.extend_from_slice(&entry.crc32.to_be_bytes());
    }

    // 5. 4-byte offset table + 8-byte extended offset table (if any >= 2 GiB)
    let mut extended_offsets = Vec::new();
    for entry in entries.iter() {
        if entry.pack_offset < 0x8000_0000 {
            buf.extend_from_slice(&(entry.pack_offset as u32).to_be_bytes());
        } else {
            let ext_idx = extended_offsets.len() as u32;
            buf.extend_from_slice(&(0x8000_0000u32 | ext_idx).to_be_bytes());
            extended_offsets.push(entry.pack_offset);
        }
    }

    for ext_off in extended_offsets {
        buf.extend_from_slice(&ext_off.to_be_bytes());
    }

    // 6. Trailers: 20-byte pack SHA-1 + 20-byte idx SHA-1
    buf.extend_from_slice(pack_checksum.as_bytes());

    let mut hasher = Sha1::new();
    hasher.update(&buf);
    let idx_digest: [u8; 20] = hasher.finalize().into();
    let idx_checksum = ObjectId::from_bytes(idx_digest);
    buf.extend_from_slice(idx_checksum.as_bytes());

    write_immutable_atomic(idx_path, &buf)?;
    Ok(idx_checksum)
}
