use std::env;

use clap::Args;
use gir_core::storage::fsck::gc_repository;
use gir_core::{GirError, Repository};

#[derive(Debug, Args)]
pub struct GcArgs {
    /// Prune unreachable objects during garbage collection
    #[arg(long)]
    pub prune: bool,

    /// Run quietly
    #[arg(short = 'q', long)]
    pub quiet: bool,
}

pub fn run(args: GcArgs) -> Result<(), GirError> {
    let cwd = env::current_dir()?;
    let repo = Repository::discover(cwd)?;
    let report = gc_repository(&repo, args.prune)?;

    if !args.quiet {
        if let Some(sha) = report.pack_checksum {
            eprintln!(
                "Packed {} objects into pack-{} (removed {} loose objects).",
                report.packed_objects, sha, report.removed_loose_objects
            );
        }
    }

    Ok(())
}
