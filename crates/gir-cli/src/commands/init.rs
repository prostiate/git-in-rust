use std::env;
use std::path::PathBuf;

use clap::Args;
use gir_core::{GirError, Repository};

#[derive(Debug, Args)]
pub struct InitArgs {
    /// Create a bare repository (no working directory).
    #[arg(long)]
    pub bare: bool,

    /// Directory to initialize (defaults to current working directory).
    #[arg(default_value = ".")]
    pub path: PathBuf,
}

pub fn run(args: InitArgs) -> Result<(), GirError> {
    let target = if args.path.is_relative() {
        env::current_dir()?.join(&args.path)
    } else {
        args.path
    };

    let existed = if args.bare {
        target.join("HEAD").is_file()
    } else {
        target.join(".git/HEAD").is_file()
    };

    let repo = Repository::init(&target, args.bare)?;
    if existed {
        println!(
            "Reinitialized existing Git repository in {}/",
            repo.git_dir().display()
        );
    } else {
        println!(
            "Initialized empty Git repository in {}/",
            repo.git_dir().display()
        );
    }
    Ok(())
}
