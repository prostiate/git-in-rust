use std::env;

use clap::Args;
use gir_core::index::{write_index_to_tree, GitIndex};
use gir_core::{GirError, Repository};

#[derive(Debug, Args)]
pub struct WriteTreeArgs {}

pub fn run(_args: WriteTreeArgs) -> Result<(), GirError> {
    let cwd = env::current_dir()?;
    let repo = Repository::discover(&cwd)?;
    let mut index = GitIndex::read(repo.git_dir())?;
    let tree_oid = write_index_to_tree(&mut index, repo.odb())?;
    index.write(repo.git_dir())?;
    println!("{tree_oid}");
    Ok(())
}
