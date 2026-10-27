use std::env;

use clap::Args;
use gir_core::branch_ops::{create_branch, delete_branch, list_branches, rename_branch};
use gir_core::{GirError, Repository};

#[derive(Debug, Args)]
pub struct BranchArgs {
    /// List both remote-tracking branches and local branches
    #[arg(short = 'a', long = "all")]
    pub all: bool,

    /// List the remote-tracking branches
    #[arg(short = 'r', long = "remotes")]
    pub remotes: bool,

    /// Show sha1 and commit subject line for each head
    #[arg(short = 'v', long = "verbose")]
    pub verbose: bool,

    /// Delete a branch (must be fully merged in HEAD)
    #[arg(short = 'd', long = "delete")]
    pub delete: bool,

    /// Delete a branch irrespective of its merged status (`-D`)
    #[arg(short = 'D')]
    pub force_delete: bool,

    /// Move/rename a branch and its reflog
    #[arg(short = 'm', long = "move")]
    pub rename: bool,

    /// Move/rename a branch even if the new branch name already exists (`-M`)
    #[arg(short = 'M')]
    pub force_rename: bool,

    /// Force branch creation or operation even if branch exists
    #[arg(short = 'f', long = "force")]
    pub force: bool,

    /// Branch name(s) or `<branch-name> [<start-point>]`
    pub names: Vec<String>,
}

pub fn run(args: BranchArgs) -> Result<(), GirError> {
    let cwd = env::current_dir()?;
    let repo = Repository::discover(&cwd)?;

    // 1. Delete branch (`-d` / `-D`)
    if args.delete || args.force_delete {
        if args.names.is_empty() {
            return Err(GirError::InvalidArgument(
                "branch name required".to_string(),
            ));
        }
        let force = args.force_delete || args.force;
        for name in &args.names {
            let oid = delete_branch(&repo, name, force)?;
            println!("Deleted branch {name} (was {}).", oid.short_hex());
        }
        return Ok(());
    }

    // 2. Rename branch (`-m` / `-M`)
    if args.rename || args.force_rename {
        let force = args.force_rename || args.force;
        match args.names.as_slice() {
            [new_name] => rename_branch(&repo, None, new_name, force),
            [old_name, new_name] => rename_branch(&repo, Some(old_name), new_name, force),
            _ => Err(GirError::InvalidArgument(
                "usage: gir branch (-m | -M) [<old-branch>] <new-branch>".to_string(),
            )),
        }
    } else if !args.names.is_empty() {
        // 3. Create branch (`gir branch <name> [<start-point>]`)
        let name = &args.names[0];
        let start_point = args.names.get(1).map(String::as_str);
        create_branch(&repo, name, start_point, args.force)?;
        Ok(())
    } else {
        // 4. List branches
        let entries = list_branches(&repo, args.all, args.remotes)?;
        let max_name_len = entries.iter().map(|e| e.name.len()).max().unwrap_or(0);
        for entry in entries {
            let prefix = if entry.is_current { "* " } else { "  " };
            if args.verbose {
                println!(
                    "{prefix}{:<width$} {} {}",
                    entry.name,
                    entry.oid.short_hex(),
                    entry.subject,
                    width = max_name_len
                );
            } else {
                println!("{prefix}{}", entry.name);
            }
        }
        Ok(())
    }
}
