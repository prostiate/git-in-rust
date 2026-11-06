mod commands;

use clap::{Parser, Subcommand};
use commands::{
    add, branch, cat_file, checkout, cherry_pick, clean, commit, commit_tree, config, diff, fsck,
    gc, hash_object, index_pack, init, log, ls_files, ls_tree, merge, merge_base, mv, pack_objects,
    read_tree, rebase, reflog, reset, restore, rev_parse, rm, show, stash, status, switch,
    symbolic_ref, tag, undo, unpack_objects, update_ref, write_tree,
};

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

    /// Build pack index file (.idx v2) for an existing packed archive (.pack)
    #[command(name = "index-pack")]
    IndexPack(index_pack::IndexPackArgs),

    /// Unpack objects from a packed archive streamed on standard input
    #[command(name = "unpack-objects")]
    UnpackObjects(unpack_objects::UnpackObjectsArgs),

    /// Create a packed archive of objects
    #[command(name = "pack-objects")]
    PackObjects(pack_objects::PackObjectsArgs),

    /// Verify the connectivity and validity of the objects in the database
    Fsck(fsck::FsckArgs),

    /// Cleanup unnecessary files and optimize the local repository into packfiles
    Gc(gc::GcArgs),

    /// Update the object name stored in a ref safely with compare-and-swap
    #[command(name = "update-ref")]
    UpdateRef(update_ref::UpdateRefArgs),

    /// Read and modify symbolic references
    #[command(name = "symbolic-ref")]
    SymbolicRef(symbolic_ref::SymbolicRefArgs),

    /// Pick out and massage parameters and revision expressions
    #[command(name = "rev-parse")]
    RevParse(rev_parse::RevParseArgs),

    /// Manage reflog information
    Reflog(reflog::ReflogArgs),

    /// Restore working tree, index, and HEAD from a pre-operation safety snapshot
    Undo(undo::UndoArgs),

    /// Show the working tree status
    Status(status::StatusArgs),

    /// Add file contents to the index
    Add(add::AddArgs),

    /// Remove files from the working tree and from the index
    Rm(rm::RmArgs),

    /// Move or rename a file, a directory, or a symlink
    Mv(mv::MvArgs),

    /// Show information about files in the index and the working tree
    #[command(name = "ls-files")]
    LsFiles(ls_files::LsFilesArgs),

    /// Create a tree object from the current index
    #[command(name = "write-tree")]
    WriteTree(write_tree::WriteTreeArgs),

    /// Reads tree information into the index
    #[command(name = "read-tree")]
    ReadTree(read_tree::ReadTreeArgs),

    /// Create a new commit object from a tree
    #[command(name = "commit-tree")]
    CommitTree(commit_tree::CommitTreeArgs),

    /// Record changes to the repository
    Commit(commit::CommitArgs),

    /// List, create, or delete branches
    Branch(branch::BranchArgs),

    /// Create, list, or delete a tag object
    Tag(tag::TagArgs),

    /// Show commit logs
    Log(log::LogArgs),

    /// Show various types of objects (commits, tags, trees, blobs)
    Show(show::ShowArgs),

    /// Show changes between commits, commit and working tree, etc.
    Diff(diff::DiffArgs),

    /// Switch branches
    Switch(switch::SwitchArgs),

    /// Switch branches or restore working tree files
    Checkout(checkout::CheckoutArgs),

    /// Reset current HEAD to the specified state
    Reset(reset::ResetArgs),

    /// Restore working tree files
    Restore(restore::RestoreArgs),

    /// Remove untracked files from the working tree
    Clean(clean::CleanArgs),

    /// Stash the changes in a dirty working directory away
    Stash(stash::StashArgs),

    /// Find as good common ancestors as possible for a merge
    #[command(name = "merge-base")]
    MergeBase(merge_base::MergeBaseArgs),

    /// Join two or more development histories together
    Merge(merge::MergeArgs),

    /// Apply the changes introduced by some existing commits
    #[command(name = "cherry-pick")]
    CherryPick(cherry_pick::CherryPickArgs),

    /// Reapply commits on top of another base tip
    Rebase(rebase::RebaseArgs),
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Commands::Init(args) => init::run(args),
        Commands::Config(args) => config::run(args),
        Commands::HashObject(args) => hash_object::run(args),
        Commands::CatFile(args) => cat_file::run(args),
        Commands::LsTree(args) => ls_tree::run(args),
        Commands::IndexPack(args) => index_pack::run(args),
        Commands::UnpackObjects(args) => unpack_objects::run(args),
        Commands::PackObjects(args) => pack_objects::run(args),
        Commands::Fsck(args) => fsck::run(args),
        Commands::Gc(args) => gc::run(args),
        Commands::UpdateRef(args) => update_ref::run(args),
        Commands::SymbolicRef(args) => symbolic_ref::run(args),
        Commands::RevParse(args) => rev_parse::run(args),
        Commands::Reflog(args) => reflog::run(args),
        Commands::Undo(args) => undo::run(args),
        Commands::Status(args) => status::run(args),
        Commands::Add(args) => add::run(args),
        Commands::Rm(args) => rm::run(args),
        Commands::Mv(args) => mv::run(args),
        Commands::LsFiles(args) => ls_files::run(args),
        Commands::WriteTree(args) => write_tree::run(args),
        Commands::ReadTree(args) => read_tree::run(args),
        Commands::CommitTree(args) => commit_tree::run(args),
        Commands::Commit(args) => commit::run(args),
        Commands::Branch(args) => branch::run(args),
        Commands::Tag(args) => tag::run(args),
        Commands::Log(args) => log::run(args),
        Commands::Show(args) => show::run(args),
        Commands::Diff(args) => diff::run(args),
        Commands::Switch(args) => switch::run(args),
        Commands::Checkout(args) => checkout::run(args),
        Commands::Reset(args) => reset::run(args),
        Commands::Restore(args) => restore::run(args),
        Commands::Clean(args) => clean::run(args),
        Commands::Stash(args) => stash::run(args),
        Commands::MergeBase(args) => merge_base::run(args),
        Commands::Merge(args) => merge::run(args),
        Commands::CherryPick(args) => cherry_pick::run(args),
        Commands::Rebase(args) => rebase::run(args),
    };

    if let Err(err) = result {
        eprintln!("fatal: {err}");
        std::process::exit(128);
    }
}
