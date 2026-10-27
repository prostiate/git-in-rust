use std::env;
use std::fs;

use clap::Args;
use gir_core::commit_ops::{create_commit, CommitOptions};
use gir_core::{GirError, Repository};

#[derive(Debug, Args)]
pub struct CommitArgs {
    /// Use the given `<msg>` as the commit message (multiple `-m` are concatenated as paragraphs)
    #[arg(short = 'm', long = "message")]
    pub messages: Vec<String>,

    /// Take the commit message from the given file
    #[arg(short = 'F', long = "file")]
    pub file: Option<String>,

    /// Automatically stage files that have been modified and deleted
    #[arg(short = 'a', long = "all")]
    pub all: bool,

    /// Replace the tip of the current branch by creating a new commit
    #[arg(long = "amend")]
    pub amend: bool,

    /// Allow recording a commit that has the exact same tree as its sole parent commit
    #[arg(long = "allow-empty")]
    pub allow_empty: bool,

    /// Bypass the pre-commit and commit-msg hooks
    #[arg(short = 'n', long = "no-verify")]
    pub no_verify: bool,

    /// Override the commit author (`Name <email>`)
    #[arg(long = "author")]
    pub author: Option<String>,
}

pub fn run(args: CommitArgs) -> Result<(), GirError> {
    let cwd = env::current_dir()?;
    let repo = Repository::discover(&cwd)?;

    let message = if let Some(ref file_path) = args.file {
        fs::read_to_string(file_path)?
    } else if !args.messages.is_empty() {
        args.messages.join("\n\n")
    } else if args.amend {
        // When `--amend` is passed without `-m`, reuse the current HEAD commit's message
        let head_oid = repo.refs().resolve_to_oid("HEAD")?;
        let (_, head_commit) = gir_core::graph::peel_to_commit_object(repo.odb(), &head_oid)?;
        head_commit.message
    } else {
        let merge_msg_path = repo.git_dir().join("MERGE_MSG");
        if merge_msg_path.is_file() {
            fs::read_to_string(&merge_msg_path)?
        } else {
            return Err(GirError::InvalidArgument(
                "Aborting commit: please provide a commit message via -m <msg> or -F <file>"
                    .to_string(),
            ));
        }
    };

    let opts = CommitOptions {
        message,
        all: args.all,
        amend: args.amend,
        allow_empty: args.allow_empty,
        no_verify: args.no_verify,
        author_override: args.author,
    };

    let outcome = create_commit(&repo, &opts)?;
    let root_label = if outcome.is_root {
        " (root-commit)"
    } else {
        ""
    };
    println!(
        "[{}{} {}] {}",
        outcome.branch_display,
        root_label,
        outcome.commit_oid.short_hex(),
        outcome.subject
    );
    if outcome.diff_summary.files_changed() > 0 {
        println!("{}", outcome.diff_summary.format_shortstat());
    }

    Ok(())
}
