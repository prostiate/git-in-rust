use std::collections::HashSet;
use std::env;

use clap::Args;
use gir_core::commit_ops::diff_trees;
use gir_core::graph::{build_ref_decorations, peel_to_commit_object, AsciiGraphRenderer, RevWalk};
use gir_core::object::ParsedObject;
use gir_core::{GirError, ObjectId, Repository};

#[derive(Debug, Args)]
pub struct LogArgs {
    /// Limit the number of commits to output (`-n <number>`)
    #[arg(short = 'n', long = "max-count")]
    pub max_count: Option<usize>,

    /// Shorthand for `--pretty=oneline --abbrev-commit`
    #[arg(long = "oneline")]
    pub oneline: bool,

    /// Draw a text-based graphical representation of the commit history
    #[arg(long = "graph")]
    pub graph: bool,

    /// Generate a diffstat for each commit
    #[arg(long = "stat")]
    pub stat: bool,

    /// Show commits in topological order (children strictly before parents)
    #[arg(long = "topo-order")]
    pub topo_order: bool,

    /// Do not print ref names of any commits
    #[arg(long = "no-decorate")]
    pub no_decorate: bool,

    /// Pretend as if all the refs in `refs/` along with `HEAD` are listed on the command line
    #[arg(long = "all")]
    pub all: bool,

    /// Revision arguments or ranges (`main..feature`, `HEAD~3`, etc.)
    pub revisions: Vec<String>,
}

pub fn run(args: LogArgs) -> Result<(), GirError> {
    let cwd = env::current_dir()?;
    let repo = Repository::discover(&cwd)?;

    let topo = args.topo_order || args.graph;
    let walk = if args.all && args.revisions.is_empty() {
        let all_refs = repo.refs().list_all()?;
        let mut roots = Vec::new();
        for (name, oid) in all_refs {
            if !name.starts_with("refs/gir/") {
                if let Ok((c_oid, _)) = peel_to_commit_object(repo.odb(), &oid) {
                    if !roots.contains(&c_oid) {
                        roots.push(c_oid);
                    }
                }
            }
        }
        RevWalk::build(repo.odb(), &roots, &HashSet::new(), topo)?
    } else {
        RevWalk::from_revisions(&repo, &args.revisions, topo)?
    };

    let decorations = if args.no_decorate {
        Default::default()
    } else {
        build_ref_decorations(&repo)?
    };

    let mut graph = AsciiGraphRenderer::new();
    let max_items = args.max_count.unwrap_or(usize::MAX);

    let items: Vec<_> = walk.take(max_items).collect();
    for (idx, item) in items.iter().enumerate() {
        let dec_suffix = match decorations.get(&item.oid) {
            Some(labels) if !labels.is_empty() => format!(" ({})", labels.join(", ")),
            _ => String::new(),
        };

        let (commit_pref, cont_pref, connectors) = if args.graph {
            graph.step(item.oid, &item.commit.parents)
        } else {
            (String::new(), String::new(), Vec::new())
        };

        if args.oneline {
            println!(
                "{commit_pref}{}{dec_suffix} {}",
                item.oid.short_hex(),
                item.commit.subject()
            );
            for conn in connectors {
                println!("{conn}");
            }
            continue;
        }

        if idx > 0 && !args.graph {
            println!();
        }

        println!("{commit_pref}commit {}{dec_suffix}", item.oid);
        if item.commit.parents.len() > 1 {
            let short_parents: Vec<String> = item
                .commit
                .parents
                .iter()
                .map(|p| p.short_hex())
                .collect();
            println!("{cont_pref}Merge: {}", short_parents.join(" "));
        }
        println!(
            "{cont_pref}Author: {} <{}>",
            item.commit.author.name, item.commit.author.email
        );
        println!(
            "{cont_pref}Date:   {}",
            item.commit.author.format_human_date()
        );
        println!("{cont_pref}");

        for line in item.commit.message.lines() {
            println!("{cont_pref}    {line}");
        }

        if args.stat {
            let parent_tree = first_parent_tree(&repo, &item.commit.parents)?;
            let summary = diff_trees(repo.odb(), parent_tree.as_ref(), &item.commit.tree)?;
            if summary.files_changed() > 0 {
                println!("{cont_pref}");
                for line in summary.format_stat_block().lines() {
                    println!("{cont_pref}{line}");
                }
            }
        }

        if args.graph {
            if !connectors.is_empty() {
                for conn in connectors {
                    println!("{conn}");
                }
            } else if idx + 1 < items.len() {
                println!("{}", cont_pref.trim_end());
            }
        }
    }

    Ok(())
}

pub fn first_parent_tree(
    repo: &Repository,
    parents: &[ObjectId],
) -> Result<Option<ObjectId>, GirError> {
    let Some(first) = parents.first() else {
        return Ok(None);
    };
    match repo.odb().read_parsed(first)? {
        ParsedObject::Commit(c) => Ok(Some(c.tree)),
        _ => Ok(None),
    }
}
