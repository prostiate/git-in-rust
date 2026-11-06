use std::env;

use clap::Args;
use gir_core::{GirError, ObjectId, Repository};
use gir_transport::{push_to_remote, PushOptions};

#[derive(Debug, Args)]
pub struct PushArgs {
    /// For every branch that is up to date or successfully pushed, add upstream (tracking) reference (`-u` / `--set-upstream`)
    #[arg(short = 'u', long = "set-upstream")]
    pub set_upstream: bool,

    /// Usually, the command refuses to update a remote ref that is not an ancestor of the local ref used to overwrite it (`-f` / `--force`)
    #[arg(short = 'f', long = "force", visible_alias = "force-with-lease")]
    pub force: bool,

    /// Toggle the `pre-push` hook (`--no-verify`)
    #[arg(long = "no-verify")]
    pub no_verify: bool,

    /// Delete all listed refs from the remote repository (`-d` / `--delete`)
    #[arg(short = 'd', long = "delete")]
    pub delete: bool,

    /// Remote name or URL (defaults to `"origin"`)
    pub remote: Option<String>,

    /// Refspecs to push
    pub refspecs: Vec<String>,
}

pub fn run(args: PushArgs) -> Result<(), GirError> {
    let cwd = env::current_dir()?;
    let repo = Repository::discover(&cwd)?;

    if args.delete && args.refspecs.is_empty() {
        return Err(GirError::InvalidArgument(
            "--delete doesn't make sense without any refs".to_string(),
        ));
    }

    let refspecs: Vec<String> = if args.delete {
        args.refspecs
            .iter()
            .map(|r| {
                if r.starts_with(':') {
                    r.clone()
                } else {
                    format!(":{r}")
                }
            })
            .collect()
    } else {
        args.refspecs
    };

    let outcome = push_to_remote(
        &repo,
        &PushOptions {
            remote: args.remote.clone(),
            refspecs,
            set_upstream: args.set_upstream,
            force: args.force,
            no_verify: args.no_verify,
        },
    )?;

    if outcome.up_to_date {
        eprintln!("Everything up-to-date");
        return Ok(());
    }

    eprintln!("To {}", outcome.remote_url);
    for u in &outcome.updates {
        let short_remote = u
            .remote_ref
            .strip_prefix("refs/heads/")
            .unwrap_or(&u.remote_ref);
        let short_local = u.local_branch.as_deref().unwrap_or(short_remote);

        if u.old_oid == ObjectId::ZERO {
            eprintln!(" * [new branch]      {short_local} -> {short_remote}");
        } else if u.new_oid == ObjectId::ZERO {
            eprintln!(" - [deleted]         {short_remote}");
        } else {
            eprintln!(
                "   {}..{}  {short_local} -> {short_remote}",
                u.old_oid.short_hex(),
                u.new_oid.short_hex()
            );
        }

        if args.set_upstream {
            if let Some(ref b) = u.local_branch {
                let r_name = outcome
                    .remote_name
                    .as_deref()
                    .or(args.remote.as_deref())
                    .unwrap_or("origin");
                println!("branch '{b}' set up to track '{r_name}/{short_remote}'.");
            }
        }
    }

    Ok(())
}
