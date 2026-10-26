mod commands;

use clap::{Parser, Subcommand};
use commands::{cat_file, config, hash_object, init, ls_tree};

#[derive(Debug, Parser)]
#[command(
    name = "gir",
    version,
    about = "gir — High-performance 1:1 Git implementation in Rust with zero-data-loss safety snapshots"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Create an empty Git repository or reinitialize an existing one
    Init(init::InitArgs),

    /// Get and set repository or global options
    Config(config::ConfigArgs),

    /// Compute object ID and optionally create a blob/object from a file
    #[command(name = "hash-object")]
    HashObject(hash_object::HashObjectArgs),

    /// Provide content or type and size information for repository objects
    #[command(name = "cat-file")]
    CatFile(cat_file::CatFileArgs),

    /// List the contents of a tree object
    #[command(name = "ls-tree")]
    LsTree(ls_tree::LsTreeArgs),
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Commands::Init(args) => init::run(args),
        Commands::Config(args) => config::run(args),
        Commands::HashObject(args) => hash_object::run(args),
        Commands::CatFile(args) => cat_file::run(args),
        Commands::LsTree(args) => ls_tree::run(args),
    };

    if let Err(err) = result {
        eprintln!("fatal: {err}");
        std::process::exit(128);
    }
}
