use std::fs::Metadata;
use std::os::unix::fs::MetadataExt;

use crate::hash::ObjectId;
use crate::object::tree::FileMode;

/// A single entry in `.git/index` (DIRC v2/v3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    pub ctime_sec: u32,
    pub ctime_nsec: u32,
    pub mtime_sec: u32,
    pub mtime_nsec: u32,
    pub dev: u32,
    pub ino: u32,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub file_size: u32,
    pub oid: ObjectId,
    pub flags: u16,
    pub path: String,
}

impl IndexEntry {
    /// Creates an `IndexEntry` from a working-tree file's `Metadata` and its blob `ObjectId`.
    pub fn from_metadata(path: impl Into<String>, oid: ObjectId, stage: u8, meta: &Metadata) -> Self {
        let path_str = path.into();
        let name_len = path_str.len().min(0x0fff) as u16;
        let flags = (((stage & 0x03) as u16) << 12) | name_len;

        let file_type = meta.file_type();
        let mode = if file_type.is_symlink() {
            0o120000
        } else if (meta.mode() & 0o111) != 0 {
            0o100755
        } else {
            0o100644
        };

        let file_size = if file_type.is_symlink() {
            meta.len() as u32
        } else {
            (meta.len() & 0xffff_ffff) as u32
        };

        Self {
            ctime_sec: (meta.ctime() as u64 & 0xffff_ffff) as u32,
            ctime_nsec: (meta.ctime_nsec() as u64 & 0xffff_ffff) as u32,
            mtime_sec: (meta.mtime() as u64 & 0xffff_ffff) as u32,
            mtime_nsec: (meta.mtime_nsec() as u64 & 0xffff_ffff) as u32,
            dev: (meta.dev() & 0xffff_ffff) as u32,
            ino: (meta.ino() & 0xffff_ffff) as u32,
            mode,
            uid: meta.uid(),
            gid: meta.gid(),
            file_size,
            oid,
            flags,
            path: path_str,
        }
    }

    /// Creates a synthetic `IndexEntry` (e.g. when populating `.git/index` from a `Tree` object).
    pub fn from_tree_entry(path: impl Into<String>, file_mode: FileMode, oid: ObjectId, stage: u8) -> Self {
        let path_str = path.into();
        let name_len = path_str.len().min(0x0fff) as u16;
        let flags = (((stage & 0x03) as u16) << 12) | name_len;
        let mode = match file_mode {
            FileMode::Regular => 0o100644,
            FileMode::Executable => 0o100755,
            FileMode::Symlink => 0o120000,
            FileMode::Gitlink => 0o160000,
            FileMode::Directory => 0o040000,
        };

        Self {
            ctime_sec: 0,
            ctime_nsec: 0,
            mtime_sec: 0,
            mtime_nsec: 0,
            dev: 0,
            ino: 0,
            mode,
            uid: 0,
            gid: 0,
            file_size: 0,
            oid,
            flags,
            path: path_str,
        }
    }

    /// Returns the 2-bit merge stage (`0` = normal, `1` = base, `2` = ours, `3` = theirs).
    #[inline]
    pub fn stage(&self) -> u8 {
        ((self.flags >> 12) & 0x03) as u8
    }

    /// Converts the numeric POSIX mode (`0o100644`, `0o100755`, `0o120000`, `0o160000`) to `FileMode`.
    pub fn file_mode(&self) -> FileMode {
        match self.mode {
            0o100755 => FileMode::Executable,
            0o120000 => FileMode::Symlink,
            0o160000 => FileMode::Gitlink,
            _ => FileMode::Regular,
        }
    }

    /// Fast stat-cache check comparing this index entry against a working-tree file's `Metadata`.
    /// Includes "Racy Git" protection when `index_mtime_sec` is provided.
    pub fn matches_stat(&self, meta: &Metadata, index_mtime_sec: Option<u32>) -> bool {
        if self.mtime_sec == 0 && self.ctime_sec == 0 {
            return false;
        }

        // Racy Git protection: if entry's mtime is >= the `.git/index` file's mtime, force content hash verification
        if let Some(idx_mtime) = index_mtime_sec {
            if self.mtime_sec >= idx_mtime {
                return false;
            }
        }

        let meta_mtime_sec = (meta.mtime() as u64 & 0xffff_ffff) as u32;
        let meta_mtime_nsec = (meta.mtime_nsec() as u64 & 0xffff_ffff) as u32;
        let meta_size = (meta.len() & 0xffff_ffff) as u32;

        let expected_mode = if meta.file_type().is_symlink() {
            0o120000
        } else if (meta.mode() & 0o111) != 0 {
            0o100755
        } else {
            0o100644
        };

        self.mtime_sec == meta_mtime_sec
            && self.mtime_nsec == meta_mtime_nsec
            && self.file_size == meta_size
            && self.mode == expected_mode
    }
}
