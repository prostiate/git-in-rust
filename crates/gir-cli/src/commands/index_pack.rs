use std::env;
use std::path::PathBuf;

use clap::Args;
use gir_core::pack::index_pack_file;
use gir_core::{GirError, Repository};

#[derive(Debug, Args)]
pub struct IndexPackArgs {
    /// Write the generated pack index into the specified file
    #[arg(short = 'o')]
    pub output: Option<PathBuf>,

    /// Path to the `.pack` file to index
    pub pack_file: PathBuf,
}

pub fn run(args: IndexPackArgs) -> Result<(), GirError> {
    let cwd = env::current_dir()?;
    let maybe_repo = Repository::discover(&cwd).ok();
    let odb = maybe_repo.as_ref().map(Repository::odb);

    let (_, pack_checksum, _) =
        index_pack_file(&args.pack_file, args.output.as_deref(), odb)?;
    println!("{pack_checksum}");
    Ok(())
}
