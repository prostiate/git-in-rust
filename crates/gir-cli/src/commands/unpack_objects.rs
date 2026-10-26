use std::env;
use std::io::{self, Read};

use clap::Args;
use gir_core::pack::unpack_pack_bytes;
use gir_core::{GirError, Repository};

#[derive(Debug, Args)]
pub struct UnpackObjectsArgs {
    /// Operate quietly
    #[arg(short = 'q')]
    pub quiet: bool,
}

pub fn run(args: UnpackObjectsArgs) -> Result<(), GirError> {
    let cwd = env::current_dir()?;
    let repo = Repository::discover(cwd)?;

    let mut pack_bytes = Vec::new();
    io::stdin().read_to_end(&mut pack_bytes)?;

    let unpacked = unpack_pack_bytes(&pack_bytes, repo.odb())?;
    if !args.quiet {
        eprintln!("Unpacking objects: 100% ({0}/{0}), done.", unpacked.len());
    }
    Ok(())
}
