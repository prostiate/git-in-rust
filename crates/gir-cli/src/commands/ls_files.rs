use std::env;

use clap::Args;
use gir_core::index::GitIndex;
use gir_core::status::{compute_status, StatusOptions};
use gir_core::{GirError, Repository};

#[derive(Debug, Args)]
pub struct LsFilesArgs {
    /// Show staged contents' mode bits, object name and stage number in the output
    #[arg(short = 's', long = "stage")]
    pub stage: bool,

    /// Show cached files in the output (default)
    #[arg(short = 'c', long = "cached")]
    pub cached: bool,

    /// Show other (i.e. untracked) files in the output
    #[arg(short = 'o', long = "others")]
    pub others: bool,

    /// Show only ignored files in the output
    #[arg(short = 'i', long = "ignored")]
    pub ignored: bool,

    /// Add the standard Git exclusions (`.git/info/exclude`, `.gitignore`, global ignore)
    #[arg(long = "exclude-standard")]
    pub exclude_standard: bool,
}

pub fn run(args: LsFilesArgs) -> Result<(), GirError> {
    let cwd = env::current_dir()?;
    let repo = Repository::discover(cwd)?;

    if args.others || args.ignored {
        let report = compute_status(
            &repo,
            StatusOptions {
                untracked_all: true,
                include_ignored: args.ignored,
            },
        )?;
        if args.ignored {
            for p in &report.ignored {
                println!("{}", p.trim_end_matches('/'));
            }
        } else {
            for p in &report.untracked {
                println!("{p}");
            }
        }
        return Ok(());
    }

    let index = GitIndex::read(repo.git_dir())?;
    for entry in index.entries() {
        if args.stage {
            println!(
                "{:06o} {} {}\t{}",
                entry.mode,
                entry.oid,
                entry.stage(),
                entry.path
            );
        } else {
            println!("{}", entry.path);
        }
    }

    Ok(())
}
