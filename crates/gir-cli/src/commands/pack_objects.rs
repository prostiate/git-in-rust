use std::env;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use clap::Args;
use gir_core::pack::{build_pack_bytes, write_named_pack_and_index};
use gir_core::{GirError, Repository};

#[derive(Debug, Args)]
pub struct PackObjectsArgs {
    /// Write the packed stream to standard output instead of `<base-name>-<sha>.{pack,idx}`
    #[arg(long)]
    pub stdout: bool,

    /// Pack all objects currently in the repository object database
    #[arg(long)]
    pub all: bool,

    /// Base name for the output `.pack` and `.idx` files (`<base-name>-<sha>.pack`)
    pub base_name: Option<PathBuf>,
}

pub fn run(args: PackObjectsArgs) -> Result<(), GirError> {
    let cwd = env::current_dir()?;
    let repo = Repository::discover(cwd)?;
    let odb = repo.odb();

    let oids = if args.all {
        odb.list_all_objects()?
    } else {
        let stdin = io::stdin();
        let mut list = Vec::new();
        for line in stdin.lock().lines() {
            let line = line?;
            let token = line.split_whitespace().next().unwrap_or("").trim();
            if token.is_empty() {
                continue;
            }
            let oid = repo.resolve_object(token)?;
            list.push(oid);
        }
        list
    };

    let mut objects = Vec::with_capacity(oids.len());
    for oid in oids {
        let raw = odb.read_raw(&oid)?;
        objects.push((oid, raw));
    }

    if args.stdout {
        let (pack_bytes, _, _) = build_pack_bytes(&objects)?;
        let mut out = io::stdout().lock();
        out.write_all(&pack_bytes)?;
        out.flush()?;
    } else {
        let Some(base) = args.base_name else {
            return Err(GirError::InvalidArgument(
                "gir pack-objects requires either <base-name> or --stdout".to_string(),
            ));
        };
        let (_, _, pack_sha, _) = write_named_pack_and_index(&objects, &base)?;
        println!("{pack_sha}");
    }

    Ok(())
}
