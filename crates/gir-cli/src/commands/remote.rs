use std::collections::BTreeMap;
use std::env;
use std::fs;

use clap::{Args, Subcommand};
use gir_core::refs::{PackedRefEntry, PackedRefs, RefTarget};
use gir_core::{GirError, Repository};

#[derive(Debug, Args)]
pub struct RemoteArgs {
    /// Be a little more verbose and show remote url after name (`-v` / `--verbose`)
    #[arg(short = 'v', long = "verbose")]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Option<RemoteSubcommand>,
}

#[derive(Debug, Subcommand)]
pub enum RemoteSubcommand {
    /// Add a remote named `<name>` for the repository at `<url>`
    Add {
        name: String,
        url: String,
    },

    /// Remove the remote named `<name>`
    #[command(visible_alias = "rm")]
    Remove {
        name: String,
    },

    /// Rename the remote named `<old>` to `<new>`
    Rename {
        old_name: String,
        new_name: String,
    },

    /// Change URLs for the remote
    #[command(name = "set-url")]
    SetUrl {
        name: String,
        url: String,
    },

    /// Give some information about the remote `<name>`
    #[command(name = "get-url")]
    GetUrl {
        name: String,
    },
}

pub fn run(args: RemoteArgs) -> Result<(), GirError> {
    let cwd = env::current_dir()?;
    let repo = Repository::discover(&cwd)?;

    match args.command {
        None => {
            let remotes = list_configured_remotes(&repo)?;
            for (name, url) in remotes {
                if args.verbose {
                    println!("{name}\t{url} (fetch)");
                    println!("{name}\t{url} (push)");
                } else {
                    println!("{name}");
                }
            }
            Ok(())
        }
        Some(RemoteSubcommand::Add { name, url }) => {
            let mut cfg = repo.local_config()?;
            if cfg.get(&format!("remote.{name}.url")).is_some() {
                return Err(GirError::InvalidArgument(format!(
                    "remote {name} already exists."
                )));
            }
            cfg.set(&format!("remote.{name}.url"), &url)?;
            cfg.set(
                &format!("remote.{name}.fetch"),
                &format!("+refs/heads/*:refs/remotes/{name}/*"),
            )?;
            cfg.save_to(repo.git_dir().join("config"))
        }
        Some(RemoteSubcommand::Remove { name }) => {
            let mut cfg = repo.local_config()?;
            if cfg.get(&format!("remote.{name}.url")).is_none() {
                return Err(GirError::InvalidArgument(format!(
                    "No such remote: '{name}'"
                )));
            }
            let _ = cfg.remove_section("remote", Some(&name));

            // Remove branch tracking config entries pointing to this remote
            let branch_remote_keys: Vec<String> = cfg
                .entries()
                .into_iter()
                .filter_map(|(k, v)| {
                    if k.starts_with("branch.") && k.ends_with(".remote") && v == name {
                        Some(k)
                    } else {
                        None
                    }
                })
                .collect();
            for r_key in branch_remote_keys {
                let _ = cfg.unset(&r_key);
                if let Some(prefix) = r_key.strip_suffix(".remote") {
                    let _ = cfg.unset(&format!("{prefix}.merge"));
                }
            }
            cfg.save_to(repo.git_dir().join("config"))?;

            // Remove loose remote-tracking refs under `.git/refs/remotes/<name>`
            let remotes_dir = repo.git_dir().join("refs/remotes").join(&name);
            if remotes_dir.is_dir() {
                let _ = fs::remove_dir_all(&remotes_dir);
            }
            let logs_dir = repo.git_dir().join("logs/refs/remotes").join(&name);
            if logs_dir.is_dir() {
                let _ = fs::remove_dir_all(&logs_dir);
            }

            // Remove packed remote-tracking refs under `refs/remotes/<name>/`
            let mut packed = PackedRefs::load(repo.git_dir())?;
            let prefix = format!("refs/remotes/{name}/");
            let packed_names: Vec<String> = packed
                .entries()
                .keys()
                .filter(|k| k.starts_with(&prefix))
                .cloned()
                .collect();
            if !packed_names.is_empty() {
                for pn in packed_names {
                    packed.remove(&pn);
                }
                packed.save(repo.git_dir())?;
            }
            Ok(())
        }
        Some(RemoteSubcommand::Rename { old_name, new_name }) => {
            let mut cfg = repo.local_config()?;
            if cfg.get(&format!("remote.{new_name}.url")).is_some() {
                return Err(GirError::InvalidArgument(format!(
                    "remote {new_name} already exists."
                )));
            }
            let Some(kv) = cfg.remove_section("remote", Some(&old_name)) else {
                return Err(GirError::InvalidArgument(format!(
                    "No such remote: '{old_name}'"
                )));
            };
            for (k, v) in kv {
                let updated_val = if k == "fetch" {
                    v.replace(
                        &format!(":refs/remotes/{old_name}/"),
                        &format!(":refs/remotes/{new_name}/"),
                    )
                } else {
                    v
                };
                cfg.set(&format!("remote.{new_name}.{k}"), updated_val)?;
            }

            // Update any `branch.<b_name>.remote == old_name` to `new_name`
            let branch_remote_keys: Vec<String> = cfg
                .entries()
                .into_iter()
                .filter_map(|(k, v)| {
                    if k.starts_with("branch.") && k.ends_with(".remote") && v == old_name {
                        Some(k)
                    } else {
                        None
                    }
                })
                .collect();
            for r_key in branch_remote_keys {
                cfg.set(&r_key, &new_name)?;
            }
            cfg.save_to(repo.git_dir().join("config"))?;

            let old_remotes_dir = repo.git_dir().join("refs/remotes").join(&old_name);
            let new_remotes_dir = repo.git_dir().join("refs/remotes").join(&new_name);
            if old_remotes_dir.is_dir() {
                if let Some(p) = new_remotes_dir.parent() {
                    let _ = fs::create_dir_all(p);
                }
                let _ = fs::rename(&old_remotes_dir, &new_remotes_dir);
            }
            let old_logs_dir = repo.git_dir().join("logs/refs/remotes").join(&old_name);
            let new_logs_dir = repo.git_dir().join("logs/refs/remotes").join(&new_name);
            if old_logs_dir.is_dir() {
                if let Some(p) = new_logs_dir.parent() {
                    let _ = fs::create_dir_all(p);
                }
                let _ = fs::rename(&old_logs_dir, &new_logs_dir);
            }

            // Update `refs/remotes/<new_name>/HEAD` symbolic ref if present
            let head_sym_ref = format!("refs/remotes/{new_name}/HEAD");
            if let Ok(Some(RefTarget::Symbolic(sym))) = repo.refs().read_raw(&head_sym_ref) {
                let old_prefix = format!("refs/remotes/{old_name}/");
                if let Some(rest) = sym.strip_prefix(&old_prefix) {
                    let _ = repo.refs().set_symbolic_ref(
                        &head_sym_ref,
                        &format!("refs/remotes/{new_name}/{rest}"),
                        None,
                        None,
                    );
                }
            }

            // Migrate `refs/remotes/<old_name>/*` in `packed-refs`
            let mut packed = PackedRefs::load(repo.git_dir())?;
            let old_prefix = format!("refs/remotes/{old_name}/");
            let to_migrate: Vec<(String, PackedRefEntry)> = packed
                .entries()
                .iter()
                .filter(|(k, _)| k.starts_with(&old_prefix))
                .map(|(k, e)| (k.clone(), e.clone()))
                .collect();
            if !to_migrate.is_empty() {
                for (old_k, entry) in to_migrate {
                    packed.remove(&old_k);
                    if let Some(rest) = old_k.strip_prefix(&old_prefix) {
                        packed.insert(
                            format!("refs/remotes/{new_name}/{rest}"),
                            entry.oid,
                            entry.peeled,
                        );
                    }
                }
                packed.save(repo.git_dir())?;
            }
            Ok(())
        }
        Some(RemoteSubcommand::SetUrl { name, url }) => {
            let mut cfg = repo.local_config()?;
            if cfg.get(&format!("remote.{name}.url")).is_none() {
                return Err(GirError::InvalidArgument(format!(
                    "No such remote '{name}'"
                )));
            }
            cfg.set(&format!("remote.{name}.url"), &url)?;
            cfg.save_to(repo.git_dir().join("config"))
        }
        Some(RemoteSubcommand::GetUrl { name }) => {
            let cfg = repo.config()?;
            let Some(url) = cfg.get(&format!("remote.{name}.url")) else {
                return Err(GirError::InvalidArgument(format!(
                    "No such remote '{name}'"
                )));
            };
            println!("{url}");
            Ok(())
        }
    }
}

pub fn list_configured_remotes(repo: &Repository) -> Result<BTreeMap<String, String>, GirError> {
    let cfg = repo.config()?;
    let mut map = BTreeMap::new();
    for (full_key, val) in cfg.entries() {
        if let Some(rest) = full_key.strip_prefix("remote.") {
            if let Some(remote_name) = rest.strip_suffix(".url") {
                map.insert(remote_name.to_string(), val.to_string());
            }
        }
    }
    Ok(map)
}
