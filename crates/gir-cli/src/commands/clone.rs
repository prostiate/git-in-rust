use std::env;
use std::path::PathBuf;

use clap::Args;
use gir_core::GirError;
use gir_transport::clone_repository;

#[derive(Debug, Args)]
pub struct CloneArgs {
    /// Make a bare Git repository (`--bare`)
    #[arg(long = "bare")]
    pub bare: bool,

    /// The (possibly remote) repository to clone from
    pub repository: String,

    /// The name of a new directory to clone into
    pub directory: Option<PathBuf>,
}

pub fn run(args: CloneArgs) -> Result<(), GirError> {
    let cwd = env::current_dir()?;
    let dest_dir = match args.directory {
        Some(d) => {
            if d.is_absolute() {
                d
            } else {
                cwd.join(d)
            }
        }
        None => {
            let derived = derive_repo_dir_name(&args.repository, args.bare);
            cwd.join(derived)
        }
    };

    let dir_display = dest_dir
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| dest_dir.display().to_string());

    if args.bare {
        eprintln!("Cloning into bare repository '{dir_display}'...");
    } else {
        eprintln!("Cloning into '{dir_display}'...");
    }

    clone_repository(&args.repository, &dest_dir, args.bare)?;
    Ok(())
}

fn derive_repo_dir_name(url: &str, bare: bool) -> String {
    let trimmed = url.trim_end_matches('/');
    let last_segment = trimmed
        .rsplit(&['/', ':'][..])
        .next()
        .unwrap_or("repository");
    let base = last_segment.strip_suffix(".git").unwrap_or(last_segment);
    if bare {
        format!("{base}.git")
    } else {
        base.to_string()
    }
}
