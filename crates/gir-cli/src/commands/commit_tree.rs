use std::env;
use std::io::{self, Read};

use clap::Args;
use gir_core::commit_ops::normalize_commit_message;
use gir_core::graph::peel_to_commit_object;
use gir_core::object::commit::Commit;
use gir_core::revision::parse_revision;
use gir_core::{GirError, ObjectId, Repository};

#[derive(Debug, Args)]
pub struct CommitTreeArgs {
    /// Existing tree object ID or revision
    pub tree: String,

    /// Parent commit object IDs (`-p <parent>`)
    #[arg(short = 'p', num_args = 1)]
    pub parents: Vec<String>,

    /// Commit message paragraph(s) (`-m <message>`)
    #[arg(short = 'm', num_args = 1)]
    pub messages: Vec<String>,
}

pub fn run(args: CommitTreeArgs) -> Result<(), GirError> {
    let cwd = env::current_dir()?;
    let repo = Repository::discover(&cwd)?;

    let tree_spec = format!("{}^{{tree}}", args.tree);
    let tree_oid = parse_revision(&repo, &tree_spec)
        .or_else(|_| parse_revision(&repo, &args.tree))?;

    let mut parent_oids: Vec<ObjectId> = Vec::with_capacity(args.parents.len());
    for p_spec in &args.parents {
        let raw_oid = parse_revision(&repo, p_spec)?;
        let (c_oid, _) = peel_to_commit_object(repo.odb(), &raw_oid)?;
        parent_oids.push(c_oid);
    }

    let raw_message = if !args.messages.is_empty() {
        args.messages.join("\n\n")
    } else {
        let mut buf = String::new();
        io::stdin().read_to_string(&mut buf)?;
        buf
    };
    let message = normalize_commit_message(&raw_message);

    let author = repo.resolve_author_signature()?;
    let committer = repo.resolve_committer_signature()?;

    let commit = Commit {
        tree: tree_oid,
        parents: parent_oids,
        author,
        committer,
        message,
    };

    let commit_oid = repo.odb().write_object(&commit.to_raw())?;
    println!("{commit_oid}");
    Ok(())
}
