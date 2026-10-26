use std::env;

use clap::Args;
use gir_core::{GirError, GitConfig, Repository};

#[derive(Debug, Args)]
pub struct ConfigArgs {
    /// Use global configuration file (`~/.gitconfig`).
    #[arg(long, conflicts_with = "local")]
    pub global: bool,

    /// Use repository configuration file (`.git/config`).
    #[arg(long, conflicts_with = "global")]
    pub local: bool,

    /// List all configuration variables and values.
    #[arg(short, long)]
    pub list: bool,

    /// Get the value for a given key.
    #[arg(long)]
    pub get: bool,

    /// Remove the variable matching the key.
    #[arg(long)]
    pub unset: bool,

    /// Configuration key (e.g., `user.name`).
    pub key: Option<String>,

    /// Value to set for `key`.
    pub value: Option<String>,
}

pub fn run(args: ConfigArgs) -> Result<(), GirError> {
    if args.list {
        let cfg = load_target_config(&args)?;
        for (k, v) in cfg.entries() {
            println!("{k}={v}");
        }
        return Ok(());
    }

    let Some(ref key) = args.key else {
        return Err(GirError::InvalidArgument(
            "gir config requires a key or --list".to_string(),
        ));
    };

    if args.unset {
        let (mut cfg, path) = load_mutable_config(&args)?;
        cfg.unset(key)?;
        cfg.save_to(&path)?;
        return Ok(());
    }

    if let Some(ref val) = args.value {
        if args.get {
            return Err(GirError::InvalidArgument(
                "--get cannot be combined with a value to set".to_string(),
            ));
        }
        let (mut cfg, path) = load_mutable_config(&args)?;
        cfg.set(key, val.clone())?;
        cfg.save_to(&path)?;
        return Ok(());
    }

    // Read mode (`gir config <key>` or `gir config --get <key>`)
    let cfg = load_target_config(&args)?;
    if let Some(val) = cfg.get(key) {
        println!("{val}");
        Ok(())
    } else {
        std::process::exit(1);
    }
}

fn load_target_config(args: &ConfigArgs) -> Result<GitConfig, GirError> {
    if args.global {
        let path = GitConfig::global_config_path()
            .ok_or_else(|| GirError::Config("HOME environment variable is not set".to_string()))?;
        return GitConfig::load(path);
    }
    if args.local {
        let repo = Repository::discover(env::current_dir()?)?;
        return repo.local_config();
    }
    if let Ok(repo) = Repository::discover(env::current_dir()?) {
        repo.config()
    } else {
        GitConfig::load_cascaded(None)
    }
}

fn load_mutable_config(args: &ConfigArgs) -> Result<(GitConfig, std::path::PathBuf), GirError> {
    if args.global {
        let path = GitConfig::global_config_path()
            .ok_or_else(|| GirError::Config("HOME environment variable is not set".to_string()))?;
        let cfg = GitConfig::load(&path)?;
        return Ok((cfg, path));
    }

    let repo = Repository::discover(env::current_dir()?)?;
    let path = repo.git_dir().join("config");
    let cfg = GitConfig::load(&path)?;
    Ok((cfg, path))
}
