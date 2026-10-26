pub mod delta;
pub mod index;
pub mod reader;
pub mod writer;

use std::path::Path;

use crate::error::GirError;
use crate::hash::ObjectId;
use crate::object::RawObject;
use crate::storage::ObjectDatabase;

pub use index::{write_idx_v2, PackIndexEntry, PackIndexV2};
pub use reader::{DecodedPackEntry, PackEntryKind, PackFile};
pub use writer::{
    build_pack_bytes, index_pack_bytes, index_pack_file, unpack_pack_bytes,
    write_named_pack_and_index, write_pack_and_index,
};

/// Paired `.idx` v2 and `.pack` v2 files inside `.git/objects/pack/`.
#[derive(Debug, Clone)]
pub struct PackBundle {
    pub index: PackIndexV2,
    pub pack: PackFile,
}

impl PackBundle {
    /// Opens a paired `.idx` and `.pack` bundle given the path to the `.idx` file.
    pub fn open_from_idx(idx_path: impl AsRef<Path>) -> Result<Self, GirError> {
        let idx_path = idx_path.as_ref();
        let pack_path = idx_path.with_extension("pack");
        if !pack_path.is_file() {
            return Err(GirError::CorruptPack {
                path: pack_path,
                reason: "companion .pack file missing for .idx".to_string(),
            });
        }

        let index = PackIndexV2::open(idx_path)?;
        let pack = PackFile::open(&pack_path)?;

        if index.pack_checksum() != pack.checksum() {
            return Err(GirError::CorruptPack {
                path: idx_path.to_path_buf(),
                reason: format!(
                    "pack checksum mismatch between .idx ({}) and .pack ({})",
                    index.pack_checksum(),
                    pack.checksum()
                ),
            });
        }

        Ok(Self { index, pack })
    }

    /// Returns `true` if `id` is indexed in this pack bundle.
    pub fn contains(&self, id: &ObjectId) -> bool {
        matches!(self.index.lookup(id), Ok(Some(_)))
    }

    /// Looks up `id` in the `.idx` and decodes its `RawObject` from `.pack` if present.
    pub fn read_object(
        &self,
        id: &ObjectId,
        odb: Option<&ObjectDatabase>,
    ) -> Result<Option<RawObject>, GirError> {
        let Some(entry) = self.index.lookup(id)? else {
            return Ok(None);
        };
        let raw = self.pack.read_object_at(entry.pack_offset, odb)?;
        Ok(Some(raw))
    }
}
