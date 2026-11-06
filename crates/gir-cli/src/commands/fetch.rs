use std::env;

use clap::Args;
use gir_core::{GirError, ObjectId, Repository};
use gir_transport::fetch_from_remote;

#[derive(Debug, Args)]
pub struct FetchArgs {
    /// Remote name or URL (defaults to `"origin"`)
    pub remote: Option<String>,

    /// Optional refspecs to fetch
    pub refspecs: Vec<String>,
}

pub fn run(args: FetchArgs) -> Result<(), GirError> {
    let cwd = env::current_dir()?;
    let repo = Repository::discover(&cwd)?;

    let remote = args.remote.as_deref().unwrap_or("origin");
    let outcome = fetch_from_remote(&repo, remote, &args.refspecs)?;

    let mut printed_header = false;
    for u in &outcome.updated_refs {
        if u.old_oid == u.new_oid {
            continue;
        }
        if !printed_header {
            eprintln!("From {}", outcome.remote_url);
            printed_header = true;
        }
        let short_remote = u
            .remote_ref
            .strip_prefix("refs/heads/")
            .or_else(|| u.remote_ref.strip_prefix("refs/tags/"))
            .unwrap_or(&u.remote_ref);
        let short_tracking = u
            .local_tracking_ref
            .as_deref()
            .and_then(|t| t.strip_prefix("refs/remotes/"))
            .unwrap_or(short_remote);

        if u.old_oid == ObjectId::ZERO {
            eprintln!(" * [new branch]      {short_remote} -> {short_tracking}");
        } else {
            eprintln!(
                "   {}..{}  {short_remote} -> {short_tracking}",
                u.old_oid.short_hex(),
                u.new_oid.short_hex()
            );
        }
    }

    Ok(())
}
