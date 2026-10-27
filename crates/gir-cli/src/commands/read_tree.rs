use std::env;

use clap::Args;
use gir_core::index::GitIndex;
use gir_core::revision::parse_revision;
use gir_core::{GirError, Repository};

#[derive(Debug, Args)]
pub struct ReadTreeArgs {
    /// The tree-ish object to read into the index
    pub tree_ish: String,
}

pub fn run(args: ReadTreeArgs) -> Result<(), GirError> {
    let cwd = env::current_dir()?;
    let repo = Repository::discover(&cwd)?;
    let tree_spec = format!("{}^{{tree}}", args.tree_ish);
    let tree_oid = parse_revision(&repo, &tree_spec)
        .or_else(|_| parse_revision(&repo, &args.tree_ish))?;

    let mut index = GitIndex::read(repo.git_dir())?;
    index.populate_from_tree(repo.odb(), &tree_oid)?;
    index.write(repo.git_dir())?;
    Ok(())
}
