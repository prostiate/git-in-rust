use std::collections::BTreeMap;

use crate::error::GirError;
use crate::hash::ObjectId;
use crate::index::entry::IndexEntry;
use crate::index::extensions::IndexExtension;
use crate::index::GitIndex;
use crate::object::tree::{FileMode, Tree, TreeEntry};
use crate::storage::ObjectDatabase;

#[derive(Default)]
struct DirNode<'a> {
    files: Vec<(&'a str, &'a IndexEntry)>,
    subdirs: BTreeMap<String, DirNode<'a>>,
}

struct BuiltTreeNode {
    name: String,
    oid: ObjectId,
    entry_count: usize,
    children: Vec<BuiltTreeNode>,
}

impl<'a> DirNode<'a> {
    fn insert(&mut self, rel_path: &'a str, entry: &'a IndexEntry) {
        if let Some((dir_name, rest)) = rel_path.split_once('/') {
            self.subdirs
                .entry(dir_name.to_string())
                .or_default()
                .insert(rest, entry);
        } else if !rel_path.is_empty() {
            self.files.push((rel_path, entry));
        }
    }

    fn build_bottom_up(
        &self,
        name: String,
        odb: &ObjectDatabase,
    ) -> Result<BuiltTreeNode, GirError> {
        let mut tree_entries = Vec::with_capacity(self.files.len() + self.subdirs.len());
        let mut total_entries = self.files.len();
        let mut children = Vec::with_capacity(self.subdirs.len());

        for &(file_name, entry) in &self.files {
            tree_entries.push(TreeEntry::new(
                entry.file_mode(),
                file_name.to_string(),
                entry.oid,
            ));
        }

        for (subdir_name, subdir_node) in &self.subdirs {
            let built_child = subdir_node.build_bottom_up(subdir_name.clone(), odb)?;
            total_entries += built_child.entry_count;
            tree_entries.push(TreeEntry::new(
                FileMode::Directory,
                subdir_name.clone(),
                built_child.oid,
            ));
            children.push(built_child);
        }

        let tree = Tree::new(tree_entries);
        let oid = odb.write_object(&tree.to_raw())?;

        Ok(BuiltTreeNode {
            name,
            oid,
            entry_count: total_entries,
            children,
        })
    }
}

impl BuiltTreeNode {
    /// Appends this node and its descendants in pre-order to a Git `TREE` cache-tree extension buffer.
    ///
    /// Format per node (`cache-tree.c`):
    /// `<name>\0<entry_count> <subtree_count>\n<20-byte-raw-oid>`
    fn encode_cache_tree(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(self.name.as_bytes());
        buf.push(0);
        let counts = format!("{} {}\n", self.entry_count, self.children.len());
        buf.extend_from_slice(counts.as_bytes());
        buf.extend_from_slice(self.oid.as_bytes());

        for child in &self.children {
            child.encode_cache_tree(buf);
        }
    }
}

/// Builds a hierarchical Merkle `Tree` in `odb` from the stage-0 entries of `index`,
/// updates the `TREE` cache-tree extension inside `index`, and returns the root `Tree` `ObjectId`.
pub fn write_index_to_tree(
    index: &mut GitIndex,
    odb: &ObjectDatabase,
) -> Result<ObjectId, GirError> {
    let (root_oid, cache_tree_bytes) = build_tree_from_entries(index, odb)?;
    index.set_cache_tree_extension(cache_tree_bytes);
    Ok(root_oid)
}

/// Builds a hierarchical Merkle `Tree` in `odb` from `index` without mutating `index`.
pub fn build_tree_from_entries(
    index: &GitIndex,
    odb: &ObjectDatabase,
) -> Result<(ObjectId, Vec<u8>), GirError> {
    if index.has_conflicts() {
        return Err(GirError::UnmergedIndexEntries);
    }

    let mut root_dir = DirNode::default();
    for entry in index.entries() {
        if entry.stage() == 0 {
            root_dir.insert(&entry.path, entry);
        }
    }

    let built_root = root_dir.build_bottom_up(String::new(), odb)?;
    let mut cache_tree_bytes = Vec::new();
    built_root.encode_cache_tree(&mut cache_tree_bytes);

    Ok((built_root.oid, cache_tree_bytes))
}

impl GitIndex {
    /// Replaces (or inserts) the `TREE` cache-tree extension in this index.
    pub fn set_cache_tree_extension(&mut self, data: Vec<u8>) {
        self.invalidate_cache_tree();
        self.extensions.insert(
            0,
            IndexExtension {
                signature: *b"TREE",
                data,
            },
        );
    }
}
