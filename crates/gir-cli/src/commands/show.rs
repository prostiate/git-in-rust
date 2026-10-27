use std::env;
use std::io::{self, Write};

use clap::Args;
use gir_core::commit_ops::{diff_trees, format_unified_file_patch};
use gir_core::graph::build_ref_decorations;
use gir_core::object::ParsedObject;
use gir_core::revision::parse_revision;
use gir_core::{GirError, ObjectId, Repository};

use super::log::first_parent_tree;

#[derive(Debug, Args)]
pub struct ShowArgs {
    /// Generate a diffstat instead of full patch output
    #[arg(long = "stat")]
    pub stat: bool,

    /// Format commit header on a single line
    #[arg(long = "oneline")]
    pub oneline: bool,

    /// Suppress diff output (`-s` / `--no-patch`)
    #[arg(short = 's', long = "no-patch")]
    pub no_patch: bool,

    /// Object or revision to show (defaults to `HEAD`)
    pub object: Option<String>,
}

pub fn run(args: ShowArgs) -> Result<(), GirError> {
    let cwd = env::current_dir()?;
    let repo = Repository::discover(&cwd)?;

    let spec = args.object.as_deref().unwrap_or("HEAD");
    let oid = parse_revision(&repo, spec)?;
    show_object(&repo, spec, &oid, &args)
}

fn show_object(
    repo: &Repository,
    spec: &str,
    oid: &ObjectId,
    args: &ShowArgs,
) -> Result<(), GirError> {
    match repo.odb().read_parsed(oid)? {
        ParsedObject::Commit(commit) => {
            let decorations = build_ref_decorations(repo).unwrap_or_default();
            let dec_suffix = match decorations.get(oid) {
                Some(labels) if !labels.is_empty() => format!(" ({})", labels.join(", ")),
                _ => String::new(),
            };

            if args.oneline {
                println!("{}{dec_suffix} {}", oid.short_hex(), commit.subject());
            } else {
                println!("commit {oid}{dec_suffix}");
                if commit.parents.len() > 1 {
                    let short_parents: Vec<String> =
                        commit.parents.iter().map(|p| p.short_hex()).collect();
                    println!("Merge: {}", short_parents.join(" "));
                }
                println!("Author: {} <{}>", commit.author.name, commit.author.email);
                println!("Date:   {}", commit.author.format_human_date());
                println!();
                for line in commit.message.lines() {
                    println!("    {line}");
                }
            }

            if !args.no_patch && commit.parents.len() <= 1 {
                let parent_tree = first_parent_tree(repo, &commit.parents)?;
                let summary = diff_trees(repo.odb(), parent_tree.as_ref(), &commit.tree)?;
                if args.stat {
                    if summary.files_changed() > 0 {
                        println!();
                        print!("{}", summary.format_stat_block());
                    }
                } else if !summary.files.is_empty() {
                    println!();
                    for change in &summary.files {
                        let patch = format_unified_file_patch(repo.odb(), change)?;
                        print!("{patch}");
                    }
                }
            }
            Ok(())
        }
        ParsedObject::Tag(tag) => {
            println!("tag {}", tag.tag_name);
            if let Some(ref tagger) = tag.tagger {
                println!("Tagger: {} <{}>", tagger.name, tagger.email);
                println!("Date:   {}", tagger.format_human_date());
            }
            println!();
            for line in tag.message.lines() {
                println!("{line}");
            }
            println!();
            show_object(repo, &tag.object.to_hex(), &tag.object, args)
        }
        ParsedObject::Tree(tree) => {
            println!("tree {spec}\n");
            for entry in tree.entries {
                if entry.mode.is_dir() {
                    println!("{}/", entry.name);
                } else {
                    println!("{}", entry.name);
                }
            }
            Ok(())
        }
        ParsedObject::Blob(blob) => {
            let mut stdout = io::stdout().lock();
            stdout.write_all(&blob.data)?;
            stdout.flush()?;
            Ok(())
        }
    }
}
