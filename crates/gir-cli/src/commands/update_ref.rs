use std::env;

use clap::Args;
use gir_core::object::commit::Signature;
use gir_core::{GirError, ObjectId, Repository};

#[derive(Debug, Args)]
pub struct UpdateRefArgs {
    /// Delete the reference instead of updating it
    #[arg(short = 'd')]
    pub delete: bool,

    /// Update <refname> directly rather than following symbolic references
    #[arg(long = "no-deref")]
    pub no_deref: bool,

    /// Reason for the update to record in the reflog
    #[arg(short = 'm', default_value = "")]
    pub message: String,

    /// Reference name (e.g. `refs/heads/main` or `HEAD`)
    pub refname: String,

    /// New object ID or revision (or expected old value when `-d` is passed)
    pub value1: Option<String>,

    /// Expected old object ID for compare-and-swap verification
    pub value2: Option<String>,
}

pub fn run(args: UpdateRefArgs) -> Result<(), GirError> {
    let cwd = env::current_dir()?;
    let repo = Repository::discover(cwd)?;

    if args.delete {
        let expected_old = match args.value1 {
            Some(ref s) => Some(parse_oid_or_rev(&repo, s)?),
            None => None,
        };
        repo.refs().delete_ref(&args.refname, expected_old)?;
        return Ok(());
    }

    let Some(ref new_str) = args.value1 else {
        return Err(GirError::InvalidArgument(
            "gir update-ref requires <newvalue>".to_string(),
        ));
    };
    let new_oid = parse_oid_or_rev(&repo, new_str)?;
    let expected_old = match args.value2 {
        Some(ref s) => Some(parse_oid_or_rev(&repo, s)?),
        None => None,
    };

    let sig = repo
        .resolve_committer_signature()
        .unwrap_or_else(|_| Signature::now("gir", "gir@localhost"));

    repo.refs().update_ref(
        &args.refname,
        new_oid,
        expected_old,
        args.no_deref,
        &sig,
        &args.message,
    )?;

    Ok(())
}

fn parse_oid_or_rev(repo: &Repository, s: &str) -> Result<ObjectId, GirError> {
    if s.chars().all(|c| c == '0') && s.len() == 40 {
        return Ok(ObjectId::ZERO);
    }
    if let Ok(oid) = ObjectId::from_hex(s) {
        return Ok(oid);
    }
    repo.resolve_object(s)
}
