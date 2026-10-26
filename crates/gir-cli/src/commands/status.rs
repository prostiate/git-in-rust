use std::env;

use clap::Args;
use gir_core::status::{compute_status, StatusOptions};
use gir_core::{GirError, Repository};

#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Give the output in the short-format
    #[arg(short = 's', long = "short")]
    pub short: bool,

    /// Give the output in an easy-to-parse format for scripts (`--porcelain` or `--porcelain=v1`)
    #[arg(long = "porcelain", num_args = 0..=1, default_missing_value = "v1")]
    pub porcelain: Option<String>,

    /// Show the branch and tracking info even in short-format
    #[arg(short = 'b', long = "branch")]
    pub branch: bool,

    /// Show untracked files (`normal` or `all`, e.g. `-uall`)
    #[arg(short = 'u', long = "untracked-files", num_args = 0..=1, default_missing_value = "all")]
    pub untracked_mode: Option<String>,

    /// Show ignored files as well
    #[arg(long = "ignored")]
    pub ignored: bool,
}

pub fn run(args: StatusArgs) -> Result<(), GirError> {
    let cwd = env::current_dir()?;
    let repo = Repository::discover(cwd)?;

    let opts = StatusOptions {
        untracked_all: matches!(args.untracked_mode.as_deref(), Some("all")),
        include_ignored: args.ignored,
    };

    let report = compute_status(&repo, opts)?;
    if args.short || args.porcelain.is_some() {
        print!("{}", report.format_porcelain(args.branch));
    } else {
        print!("{}", report.format_long());
    }

    Ok(())
}
