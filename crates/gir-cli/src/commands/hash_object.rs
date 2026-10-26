use std::env;
use std::fs;
use std::io::{self, Read};
use std::path::PathBuf;
use std::str::FromStr;

use clap::Args;
use gir_core::{GirError, ObjectId, ObjectKind, RawObject, Repository};

#[derive(Debug, Args)]
pub struct HashObjectArgs {
    /// Object type (`blob`, `tree`, `commit`, `tag`).
    #[arg(short = 't', long = "type", default_value = "blob")]
    pub kind: String,

    /// Actually write the object into the object database (`.git/objects`).
    #[arg(short = 'w', long = "write")]
    pub write: bool,

    /// Read the object from standard input instead of from a file.
    #[arg(long)]
    pub stdin: bool,

    /// Files to hash.
    pub files: Vec<PathBuf>,
}

pub fn run(args: HashObjectArgs) -> Result<(), GirError> {
    let kind = ObjectKind::from_str(&args.kind)?;
    let repo = if args.write {
        Some(Repository::discover(env::current_dir()?)?)
    } else {
        None
    };

    if args.stdin {
        let mut buf = Vec::new();
        io::stdin().read_to_end(&mut buf)?;
        let oid = hash_or_write(repo.as_ref(), kind, &buf)?;
        println!("{oid}");
    }

    for path in &args.files {
        let data = fs::read(path)?;
        let oid = hash_or_write(repo.as_ref(), kind, &data)?;
        println!("{oid}");
    }

    if !args.stdin && args.files.is_empty() {
        return Err(GirError::InvalidArgument(
            "gir hash-object requires --stdin or at least one file path".to_string(),
        ));
    }

    Ok(())
}

fn hash_or_write(
    repo: Option<&Repository>,
    kind: ObjectKind,
    data: &[u8],
) -> Result<ObjectId, GirError> {
    let raw = RawObject {
        kind,
        data: data.to_vec(),
    };
    if let Some(repo) = repo {
        repo.odb().write_object(&raw)
    } else {
        Ok(raw.id())
    }
}
