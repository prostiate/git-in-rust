use std::env;
use std::io::{self, Write};
use std::str::FromStr;

use clap::Args;
use gir_core::{GirError, ObjectKind, ParsedObject, Repository};

#[derive(Debug, Args)]
pub struct CatFileArgs {
    /// Pretty-print the contents of `<object>` based on its type.
    #[arg(short = 'p', group = "mode")]
    pub pretty: bool,

    /// Instead of the content, show the object type (`blob`, `tree`, `commit`, `tag`).
    #[arg(short = 't', group = "mode")]
    pub show_type: bool,

    /// Instead of the content, show the object size in bytes.
    #[arg(short = 's', group = "mode")]
    pub show_size: bool,

    /// Exit with zero status if `<object>` exists and is valid; non-zero otherwise.
    #[arg(short = 'e', group = "mode")]
    pub check_exists: bool,

    /// Either `<object>` (when `-p`, `-t`, `-s`, `-e` is used) or `<type>` (`blob`, `tree`, `commit`, `tag`).
    pub first: String,

    /// Target `<object>` when the first positional argument is `<type>`.
    pub second: Option<String>,
}

pub fn run(args: CatFileArgs) -> Result<(), GirError> {
    let repo = Repository::discover(env::current_dir()?)?;

    // Handle `gir cat-file <type> <object>` raw mode
    if let Some(ref target_obj) = args.second {
        let expected_kind = ObjectKind::from_str(&args.first)?;
        let oid = repo.resolve_object(target_obj)?;
        let raw = repo.odb().read_raw(&oid)?;
        if raw.kind != expected_kind {
            return Err(GirError::UnexpectedObjectType {
                expected: expected_kind.to_string(),
                found: raw.kind.to_string(),
            });
        }
        let mut stdout = io::stdout().lock();
        stdout.write_all(&raw.data)?;
        stdout.flush()?;
        return Ok(());
    }

    let object_spec = &args.first;

    if args.check_exists {
        match repo
            .resolve_object(object_spec)
            .and_then(|oid| repo.odb().read_raw(&oid))
        {
            Ok(_) => return Ok(()),
            Err(_) => std::process::exit(1),
        }
    }

    let oid = repo.resolve_object(object_spec)?;
    let raw = repo.odb().read_raw(&oid)?;

    if args.show_type {
        println!("{}", raw.kind);
        return Ok(());
    }

    if args.show_size {
        println!("{}", raw.data.len());
        return Ok(());
    }

    if args.pretty {
        match raw.parse(&oid)? {
            ParsedObject::Blob(blob) => {
                let mut stdout = io::stdout().lock();
                stdout.write_all(&blob.data)?;
                stdout.flush()?;
            }
            ParsedObject::Tree(tree) => {
                for entry in &tree.entries {
                    println!(
                        "{} {} {}\t{}",
                        entry.mode.as_padded_octal(),
                        entry.mode.object_type_str(),
                        entry.oid,
                        entry.name
                    );
                }
            }
            ParsedObject::Commit(_) | ParsedObject::Tag(_) => {
                let mut stdout = io::stdout().lock();
                stdout.write_all(&raw.data)?;
                stdout.flush()?;
            }
        }
        return Ok(());
    }

    Err(GirError::InvalidArgument(
        "gir cat-file requires one of -p, -t, -s, -e, or <type> <object>".to_string(),
    ))
}
