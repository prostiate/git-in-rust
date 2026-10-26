use std::env;

use clap::Args;
use gir_core::storage::fsck::fsck_repository;
use gir_core::{GirError, Repository};

#[derive(Debug, Args)]
pub struct FsckArgs {
    /// Check all packfiles and CRC32 checksums thoroughly
    #[arg(long, default_value_t = true)]
    pub full: bool,

    /// Enable stricter object validation
    #[arg(long)]
    pub strict: bool,
}

pub fn run(args: FsckArgs) -> Result<(), GirError> {
    let cwd = env::current_dir()?;
    let repo = Repository::discover(cwd)?;
    let report = fsck_repository(&repo, args.full)?;

    for (kind, oid) in &report.dangling_objects {
        println!("dangling {kind} {oid}");
    }

    if !report.is_ok() {
        for err in &report.errors {
            eprintln!("error: {err}");
        }
        return Err(GirError::InvalidArgument(format!(
            "fsck found {} integrity error(s)",
            report.errors.len()
        )));
    }

    Ok(())
}
