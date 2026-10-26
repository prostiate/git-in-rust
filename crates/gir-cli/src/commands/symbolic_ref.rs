use std::env;

use clap::Args;
use gir_core::object::commit::Signature;
use gir_core::{GirError, Repository};

#[derive(Debug, Args)]
pub struct SymbolicRefArgs {
    /// Do not issue an error message if <name> is not a symbolic ref
    #[arg(short = 'q', long)]
    pub quiet: bool,

    /// Shorten the symbolic ref name (e.g. `refs/heads/main` -> `main`)
    #[arg(long)]
    pub short: bool,

    /// Reason for the update to record in the reflog
    #[arg(short = 'm')]
    pub message: Option<String>,

    /// Name of the symbolic ref (typically `HEAD`)
    pub name: String,

    /// New target reference (e.g. `refs/heads/feature`)
    pub target: Option<String>,
}

pub fn run(args: SymbolicRefArgs) -> Result<(), GirError> {
    let cwd = env::current_dir()?;
    let repo = Repository::discover(cwd)?;

    if let Some(ref target_ref) = args.target {
        let sig = repo
            .resolve_committer_signature()
            .unwrap_or_else(|_| Signature::now("gir", "gir@localhost"));
        repo.refs().set_symbolic_ref(
            &args.name,
            target_ref,
            Some(&sig),
            args.message.as_deref(),
        )?;
        return Ok(());
    }

    match repo.refs().read_symbolic_ref(&args.name) {
        Ok(target) => {
            if args.short {
                println!("{}", shorten_ref_name(&target));
            } else {
                println!("{target}");
            }
            Ok(())
        }
        Err(e) => {
            if args.quiet {
                std::process::exit(1);
            }
            Err(e)
        }
    }
}

pub fn shorten_ref_name(full_ref: &str) -> &str {
    full_ref
        .strip_prefix("refs/heads/")
        .or_else(|| full_ref.strip_prefix("refs/tags/"))
        .or_else(|| full_ref.strip_prefix("refs/remotes/"))
        .or_else(|| full_ref.strip_prefix("refs/"))
        .unwrap_or(full_ref)
}
