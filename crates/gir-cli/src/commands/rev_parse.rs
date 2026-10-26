use std::env;

use clap::Args;
use gir_core::revision::{parse_revision_spec, RevisionSpec};
use gir_core::{GirError, Repository};

use super::symbolic_ref::shorten_ref_name;

#[derive(Debug, Args)]
pub struct RevParseArgs {
    /// Shorten the object name to a 7-character (or custom) unambiguous prefix
    #[arg(long)]
    pub short: bool,

    /// Verify that exactly one valid object is provided
    #[arg(long)]
    pub verify: bool,

    /// Output a non-ambiguous short name for the object's ref
    #[arg(long = "abbrev-ref")]
    pub abbrev_ref: bool,

    /// Output the `.git` directory path
    #[arg(long = "git-dir")]
    pub git_dir: bool,

    /// Output the absolute path of the top-level working directory
    #[arg(long = "show-toplevel")]
    pub show_toplevel: bool,

    /// Revision specifications to parse
    pub revs: Vec<String>,
}

pub fn run(args: RevParseArgs) -> Result<(), GirError> {
    let cwd = env::current_dir()?;
    let repo = Repository::discover(cwd)?;

    if args.git_dir {
        println!("{}", repo.git_dir().display());
    }
    if args.show_toplevel {
        if let Some(wt) = repo.worktree() {
            println!("{}", wt.display());
        }
    }

    for rev in &args.revs {
        if args.abbrev_ref {
            if rev == "HEAD" || rev == "@" {
                if let Ok(sym) = repo.refs().read_symbolic_ref("HEAD") {
                    println!("{}", shorten_ref_name(&sym));
                } else {
                    println!("HEAD");
                }
                continue;
            }
            if let Ok((full_ref, _)) = repo.refs().resolve_dwim(rev) {
                println!("{}", shorten_ref_name(&full_ref));
                continue;
            }
        }

        match parse_revision_spec(&repo, rev)? {
            RevisionSpec::Single(oid) => {
                println!("{}", format_oid(&repo, &oid, args.short));
            }
            RevisionSpec::Range { from, to } => {
                if args.verify {
                    return Err(GirError::InvalidRevision(rev.clone()));
                }
                println!("{}", format_oid(&repo, &to, args.short));
                println!("^{}", format_oid(&repo, &from, args.short));
            }
            RevisionSpec::SymmetricRange { left, right } => {
                if args.verify {
                    return Err(GirError::InvalidRevision(rev.clone()));
                }
                println!("{}", format_oid(&repo, &left, args.short));
                println!("{}", format_oid(&repo, &right, args.short));
            }
        }
    }

    Ok(())
}

fn format_oid(repo: &Repository, oid: &gir_core::ObjectId, short: bool) -> String {
    if short {
        repo.odb().abbreviate_oid(oid, 7)
    } else {
        oid.to_hex()
    }
}
