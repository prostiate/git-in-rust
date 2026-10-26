use std::env;

use clap::Args;
use gir_core::reflog::snapshot::SafetySnapshotEngine;
use gir_core::{GirError, Repository};

#[derive(Debug, Args)]
pub struct UndoArgs {
    /// List recorded safety snapshots in reverse chronological order
    #[arg(short = 'l', long)]
    pub list: bool,

    /// Capture a manual pre-operation safety snapshot with the given label
    #[arg(long = "snapshot")]
    pub snapshot_label: Option<String>,

    /// Optional snapshot SHA or `snapshot@{N}` to restore (defaults to latest snapshot)
    pub target: Option<String>,
}

pub fn run(args: UndoArgs) -> Result<(), GirError> {
    let cwd = env::current_dir()?;
    let repo = Repository::discover(cwd)?;

    if let Some(ref label) = args.snapshot_label {
        let record = SafetySnapshotEngine::capture(&repo, label)?;
        println!(
            "Captured safety snapshot {} ({})",
            &record.snapshot_oid.to_hex()[..7],
            record.operation
        );
        return Ok(());
    }

    if args.list {
        let entries = SafetySnapshotEngine::list(&repo)?;
        for (idx, entry) in entries.iter().enumerate() {
            println!(
                "{} snapshot@{{{idx}}}: {}",
                &entry.new_oid.to_hex()[..7],
                entry.message
            );
        }
        return Ok(());
    }

    let report = SafetySnapshotEngine::restore(&repo, args.target.as_deref())?;
    println!(
        "Restored snapshot {} ('{}'): {} file(s) recovered.",
        &report.restored_snapshot.to_hex()[..7],
        report.operation,
        report.restored_files
    );
    Ok(())
}
