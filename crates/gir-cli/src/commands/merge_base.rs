use std::env;
use std::process;

use clap::Args;
use gir_core::graph::is_ancestor;
use gir_core::merge::find_merge_bases;
use gir_core::revision::parse_revision;
use gir_core::{GirError, Repository};

#[derive(Debug, Args)]
pub struct MergeBaseArgs {
    /// Output all common ancestors for the merge instead of a single one
    #[arg(short = 'a', long = "all")]
    pub all: bool,

    /// Check if the first `<commit>` is an ancestor of the second `<commit>`, and exit with status 0 if true, or 1 if not
    #[arg(long = "is-ancestor")]
    pub is_ancestor: bool,

    /// First commit
    pub commit_a: String,

    /// Second commit
    pub commit_b: String,
}

pub fn run(args: MergeBaseArgs) -> Result<(), GirError> {
    let cwd = env::current_dir()?;
    let repo = Repository::discover(&cwd)?;

    let oid_a = parse_revision(&repo, &args.commit_a)?;
    let oid_b = parse_revision(&repo, &args.commit_b)?;

    if args.is_ancestor {
        if is_ancestor(repo.odb(), &oid_a, &oid_b)? {
            return Ok(());
        } else {
            process::exit(1);
        }
    }

    let bases = find_merge_bases(repo.odb(), &oid_a, &oid_b)?;
    if bases.is_empty() {
        process::exit(1);
    }

    if args.all {
        for b in bases {
            println!("{b}");
        }
    } else if let Some(first) = bases.first() {
        println!("{first}");
    }

    Ok(())
}
