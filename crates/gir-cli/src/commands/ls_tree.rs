use std::env;

use clap::Args;
use gir_core::object::tree::{FileMode, Tree};
use gir_core::{GirError, ObjectId, ParsedObject, Repository};

#[derive(Debug, Args)]
pub struct LsTreeArgs {
    /// Recurse into sub-trees.
    #[arg(short = 'r', long = "recursive")]
    pub recursive: bool,

    /// Show only the named tree entry itself, not its children (only directories).
    #[arg(short = 'd', long = "only-trees")]
    pub only_trees: bool,

    /// Show tree entries even when going to recurse them.
    #[arg(short = 't')]
    pub show_trees_when_recursing: bool,

    /// List only filenames instead of mode/type/oid columns.
    #[arg(long = "name-only")]
    pub name_only: bool,

    /// The tree-ish object (`HEAD`, branch, commit SHA, or tree SHA).
    pub tree_ish: String,
}

pub fn run(args: LsTreeArgs) -> Result<(), GirError> {
    let repo = Repository::discover(env::current_dir()?)?;
    let oid = repo.resolve_object(&args.tree_ish)?;
    let tree = peel_to_tree(&repo, &oid)?;

    print_tree(&repo, &tree, "", &args)?;
    Ok(())
}

fn peel_to_tree(repo: &Repository, oid: &ObjectId) -> Result<Tree, GirError> {
    let mut current = *oid;
    for _ in 0..10 {
        match repo.odb().read_parsed(&current)? {
            ParsedObject::Tree(tree) => return Ok(tree),
            ParsedObject::Commit(commit) => current = commit.tree,
            ParsedObject::Tag(tag) => current = tag.object,
            ParsedObject::Blob(_) => {
                return Err(GirError::UnexpectedObjectType {
                    expected: "tree".to_string(),
                    found: "blob".to_string(),
                });
            }
        }
    }
    Err(GirError::CorruptObject {
        id: oid.to_hex(),
        reason: "exceeded peel depth while resolving tree-ish".to_string(),
    })
}

fn print_tree(
    repo: &Repository,
    tree: &Tree,
    prefix: &str,
    args: &LsTreeArgs,
) -> Result<(), GirError> {
    for entry in &tree.entries {
        let full_name = if prefix.is_empty() {
            entry.name.clone()
        } else {
            format!("{prefix}/{}", entry.name)
        };

        let is_dir = entry.mode == FileMode::Directory;

        if is_dir && args.recursive && !args.only_trees {
            if args.show_trees_when_recursing {
                emit_entry(entry.mode, &entry.oid, &full_name, args.name_only);
            }
            let sub_tree = peel_to_tree(repo, &entry.oid)?;
            print_tree(repo, &sub_tree, &full_name, args)?;
        } else {
            if args.only_trees && !is_dir {
                continue;
            }
            emit_entry(entry.mode, &entry.oid, &full_name, args.name_only);
        }
    }
    Ok(())
}

fn emit_entry(mode: FileMode, oid: &ObjectId, display_path: &str, name_only: bool) {
    if name_only {
        println!("{display_path}");
    } else {
        println!(
            "{} {} {}\t{}",
            mode.as_padded_octal(),
            mode.object_type_str(),
            oid,
            display_path
        );
    }
}
