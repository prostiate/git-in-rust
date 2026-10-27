use std::env;

use clap::Args;
use gir_core::tag_ops::{create_annotated_tag, create_lightweight_tag, delete_tag, list_tags};
use gir_core::{GirError, Repository};

#[derive(Debug, Args)]
pub struct TagArgs {
    /// List tags
    #[arg(short = 'l', long = "list")]
    pub list: bool,

    /// Make an unsigned, annotated tag object
    #[arg(short = 'a', long = "annotate")]
    pub annotate: bool,

    /// Delete existing tags with the given names
    #[arg(short = 'd', long = "delete")]
    pub delete: bool,

    /// Replace an existing tag with the given name (instead of failing)
    #[arg(short = 'f', long = "force")]
    pub force: bool,

    /// Tag message (implies `-a` annotated tag)
    #[arg(short = 'm', long = "message")]
    pub message: Option<String>,

    /// Tag name (or pattern when listing) and optional `<commit>`
    pub args: Vec<String>,
}

pub fn run(args: TagArgs) -> Result<(), GirError> {
    let cwd = env::current_dir()?;
    let repo = Repository::discover(&cwd)?;

    if args.delete {
        if args.args.is_empty() {
            return Err(GirError::InvalidArgument("tag name required".to_string()));
        }
        for name in &args.args {
            let oid = delete_tag(&repo, name)?;
            println!("Deleted tag '{name}' (was {})", oid.short_hex());
        }
        return Ok(());
    }

    if args.list || args.args.is_empty() {
        let pattern = args.args.first().map(String::as_str);
        let tags = list_tags(&repo, pattern)?;
        for t in tags {
            println!("{t}");
        }
        return Ok(());
    }

    let tag_name = &args.args[0];
    let target = args.args.get(1).map(String::as_str);

    if args.annotate || args.message.is_some() {
        let Some(ref msg) = args.message else {
            return Err(GirError::InvalidArgument(
                "annotated tag (-a) requires a message via -m <msg>".to_string(),
            ));
        };
        create_annotated_tag(&repo, tag_name, target, msg, args.force)?;
    } else {
        create_lightweight_tag(&repo, tag_name, target, args.force)?;
    }

    Ok(())
}
