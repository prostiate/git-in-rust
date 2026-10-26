use std::env;

use clap::Args;
use gir_core::reflog::read_reflog;
use gir_core::{GirError, Repository};

#[derive(Debug, Args)]
pub struct ReflogArgs {
    /// Optional subcommand (`show`) or reference name (defaults to `HEAD`)
    pub arg1: Option<String>,

    /// Optional reference name when `show` is passed first
    pub arg2: Option<String>,
}

pub fn run(args: ReflogArgs) -> Result<(), GirError> {
    let cwd = env::current_dir()?;
    let repo = Repository::discover(cwd)?;

    let ref_spec = match (args.arg1.as_deref(), args.arg2.as_deref()) {
        (Some("show"), Some(r)) => r,
        (Some("show"), None) => "HEAD",
        (Some(r), _) => r,
        (None, _) => "HEAD",
    };

    let canonical_ref = if ref_spec == "HEAD" || ref_spec == "@" {
        "HEAD".to_string()
    } else if let Ok((full_ref, _)) = repo.refs().resolve_dwim(ref_spec) {
        full_ref
    } else {
        ref_spec.to_string()
    };

    let mut entries = read_reflog(repo.git_dir(), &canonical_ref)?;
    entries.reverse();

    let display_ref = canonical_ref
        .strip_prefix("refs/heads/")
        .unwrap_or(&canonical_ref);

    for (idx, entry) in entries.iter().enumerate() {
        let short_new = &entry.new_oid.to_hex()[..7];
        println!("{short_new} {display_ref}@{{{idx}}}: {}", entry.message);
    }

    Ok(())
}
