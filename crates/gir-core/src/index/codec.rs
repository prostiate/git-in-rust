use sha1::{Digest, Sha1};

use crate::error::GirError;
use crate::hash::ObjectId;
use crate::index::entry::IndexEntry;
use crate::index::extensions::IndexExtension;

const DIRC_MAGIC: &[u8; 4] = b"DIRC";
const HEADER_LEN: usize = 12;
const TRAILER_LEN: usize = 20;

/// Parses a binary `.git/index` (DIRC v2 or v3) byte slice, verifying its trailing 20-byte SHA-1 checksum.
pub fn parse_index_bytes(
    bytes: &[u8],
) -> Result<(u32, Vec<IndexEntry>, Vec<IndexExtension>), GirError> {
    if bytes.len() < HEADER_LEN + TRAILER_LEN {
        return Err(GirError::CorruptIndex(format!(
            "index file too short ({} bytes)",
            bytes.len()
        )));
    }

    if &bytes[0..4] != DIRC_MAGIC {
        return Err(GirError::CorruptIndex(
            "invalid .git/index magic header (expected 'DIRC')".to_string(),
        ));
    }

    let version = u32::from_be_bytes(bytes[4..8].try_into().unwrap());
    if version != 2 && version != 3 {
        return Err(GirError::CorruptIndex(format!(
            "unsupported .git/index version {version} (expected 2 or 3)"
        )));
    }

    let entry_count = u32::from_be_bytes(bytes[8..12].try_into().unwrap()) as usize;

    // Verify trailing 20-byte SHA-1 checksum
    let content_end = bytes.len() - TRAILER_LEN;
    let expected_sha = ObjectId::from_slice(&bytes[content_end..]).unwrap();
    let mut hasher = Sha1::new();
    hasher.update(&bytes[..content_end]);
    let actual_sha = ObjectId::from_bytes(hasher.finalize().into());
    if actual_sha != expected_sha {
        return Err(GirError::CorruptIndex(format!(
            "index SHA-1 mismatch: expected {expected_sha}, computed {actual_sha}"
        )));
    }

    let mut pos = HEADER_LEN;
    let mut entries = Vec::with_capacity(entry_count);

    for i in 0..entry_count {
        let entry_start = pos;
        if entry_start + 62 > content_end {
            return Err(GirError::CorruptIndex(format!(
                "truncated index entry #{i} at offset {entry_start}"
            )));
        }

        let ctime_sec = u32::from_be_bytes(bytes[pos..pos + 4].try_into().unwrap());
        let ctime_nsec = u32::from_be_bytes(bytes[pos + 4..pos + 8].try_into().unwrap());
        let mtime_sec = u32::from_be_bytes(bytes[pos + 8..pos + 12].try_into().unwrap());
        let mtime_nsec = u32::from_be_bytes(bytes[pos + 12..pos + 16].try_into().unwrap());
        let dev = u32::from_be_bytes(bytes[pos + 16..pos + 20].try_into().unwrap());
        let ino = u32::from_be_bytes(bytes[pos + 20..pos + 24].try_into().unwrap());
        let mode = u32::from_be_bytes(bytes[pos + 24..pos + 28].try_into().unwrap());
        let uid = u32::from_be_bytes(bytes[pos + 28..pos + 32].try_into().unwrap());
        let gid = u32::from_be_bytes(bytes[pos + 32..pos + 36].try_into().unwrap());
        let file_size = u32::from_be_bytes(bytes[pos + 36..pos + 40].try_into().unwrap());
        let oid = ObjectId::from_slice(&bytes[pos + 40..pos + 60]).unwrap();
        let raw_flags = u16::from_be_bytes(bytes[pos + 60..pos + 62].try_into().unwrap());

        pos += 62;
        // In v3, if the extended bit (0x4000) is set, 2 extra flag bytes precede the path
        if (raw_flags & 0x4000) != 0 {
            if pos + 2 > content_end {
                return Err(GirError::CorruptIndex(
                    "truncated v3 extended flags".to_string(),
                ));
            }
            pos += 2;
        }

        let Some(rel_nul) = bytes[pos..content_end].iter().position(|&b| b == 0) else {
            return Err(GirError::CorruptIndex(format!(
                "missing NUL terminator for index entry #{i} path"
            )));
        };
        let path_bytes = &bytes[pos..pos + rel_nul];
        let path = String::from_utf8(path_bytes.to_vec())
            .map_err(|_| GirError::CorruptIndex("non-UTF8 path in .git/index".to_string()))?;

        let nul_pos = pos + rel_nul;
        let consumed = (nul_pos + 1) - entry_start;
        let padded_len = (consumed + 7) & !7;
        pos = entry_start + padded_len;

        if pos > content_end {
            return Err(GirError::CorruptIndex(
                "index entry padding exceeds file bounds".to_string(),
            ));
        }

        // Normalize flags to v2 (clear extended bit 0x4000)
        let name_len = path.len().min(0x0fff) as u16;
        let flags = (raw_flags & 0xb000) | name_len;

        entries.push(IndexEntry {
            ctime_sec,
            ctime_nsec,
            mtime_sec,
            mtime_nsec,
            dev,
            ino,
            mode,
            uid,
            gid,
            file_size,
            oid,
            flags,
            path,
        });
    }

    // Parse optional extensions (each starts with 4-byte signature + 4-byte big-endian size)
    let mut extensions = Vec::new();
    while pos < content_end {
        if pos + 8 > content_end {
            return Err(GirError::CorruptIndex(
                "truncated extension header in .git/index".to_string(),
            ));
        }
        let signature: [u8; 4] = bytes[pos..pos + 4].try_into().unwrap();
        let ext_len = u32::from_be_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        pos += 8;
        if pos + ext_len > content_end {
            return Err(GirError::CorruptIndex(format!(
                "truncated extension '{}' payload in .git/index",
                String::from_utf8_lossy(&signature)
            )));
        }
        extensions.push(IndexExtension {
            signature,
            data: bytes[pos..pos + ext_len].to_vec(),
        });
        pos += ext_len;
    }

    Ok((version, entries, extensions))
}

/// Serializes `entries` and `extensions` into a canonical DIRC v2 `.git/index` binary payload
/// with 8-byte NUL padding and trailing 20-byte SHA-1 checksum.
pub fn serialize_index_bytes(
    entries: &[IndexEntry],
    extensions: &[IndexExtension],
) -> Vec<u8> {
    let mut buf = Vec::with_capacity(HEADER_LEN + entries.len() * 72 + TRAILER_LEN);

    // 1. 12-byte DIRC v2 header
    buf.extend_from_slice(DIRC_MAGIC);
    buf.extend_from_slice(&2u32.to_be_bytes());
    buf.extend_from_slice(&(entries.len() as u32).to_be_bytes());

    // 2. Sorted entries with 1..=8 NUL byte padding to 8-byte alignment
    for entry in entries {
        let entry_start = buf.len();
        buf.extend_from_slice(&entry.ctime_sec.to_be_bytes());
        buf.extend_from_slice(&entry.ctime_nsec.to_be_bytes());
        buf.extend_from_slice(&entry.mtime_sec.to_be_bytes());
        buf.extend_from_slice(&entry.mtime_nsec.to_be_bytes());
        buf.extend_from_slice(&entry.dev.to_be_bytes());
        buf.extend_from_slice(&entry.ino.to_be_bytes());
        buf.extend_from_slice(&entry.mode.to_be_bytes());
        buf.extend_from_slice(&entry.uid.to_be_bytes());
        buf.extend_from_slice(&entry.gid.to_be_bytes());
        buf.extend_from_slice(&entry.file_size.to_be_bytes());
        buf.extend_from_slice(entry.oid.as_bytes());

        let name_len = entry.path.len().min(0x0fff) as u16;
        let flags = (entry.flags & 0xb000) | name_len;
        buf.extend_from_slice(&flags.to_be_bytes());
        buf.extend_from_slice(entry.path.as_bytes());

        let unpadded_len = buf.len() - entry_start;
        let pad_len = 8 - (unpadded_len % 8);
        buf.resize(buf.len() + pad_len, 0u8);
    }

    // 3. Optional extensions
    for ext in extensions {
        buf.extend_from_slice(&ext.signature);
        buf.extend_from_slice(&(ext.data.len() as u32).to_be_bytes());
        buf.extend_from_slice(&ext.data);
    }

    // 4. Trailing 20-byte SHA-1 checksum
    let mut hasher = Sha1::new();
    hasher.update(&buf);
    let checksum: [u8; 20] = hasher.finalize().into();
    buf.extend_from_slice(&checksum);

    buf
}
