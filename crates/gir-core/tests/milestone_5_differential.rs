use std::fs;
use std::process::Command;

use gir_core::branch_ops::{create_branch, delete_branch, list_branches, rename_branch};
use gir_core::commit_ops::{create_commit, CommitOptions};
use gir_core::graph::RevWalk;
use gir_core::index::{write_index_to_tree, GitIndex, IndexEntry};
use gir_core::object::ObjectKind;
use gir_core::repository::Repository;
use gir_core::tag_ops::{create_annotated_tag, create_lightweight_tag, delete_tag, list_tags};
use tempfile::TempDir;

#[test]
fn write_tree_and_cache_tree_match_official_git_byte_for_byte() {
    let dir_gir = TempDir::new().unwrap();
    let dir_git = TempDir::new().unwrap();

    let repo_gir = Repository::init(dir_gir.path(), false).unwrap();
    let st = Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir_git.path())
        .status()
        .unwrap();
    assert!(st.success());

    let files = [
        ("README.md", "hello world\n"),
        ("foo-bar", "dash\n"),
        ("foo.txt", "dot\n"),
        ("foo/inner.txt", "inside foo dir\n"),
        ("foo0", "zero\n"),
        ("src/main.rs", "fn main() {}\n"),
        ("src/lib/mod.rs", "pub mod util;\n"),
        ("src/lib/util.rs", "pub fn add(a: i32, b: i32) -> i32 { a + b }\n"),
    ];

    let mut index = GitIndex::new();
    for (rel, content) in files {
        let p_gir = dir_gir.path().join(rel);
        let p_git = dir_git.path().join(rel);
        if let Some(parent) = p_gir.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        if let Some(parent) = p_git.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&p_gir, content).unwrap();
        fs::write(&p_git, content).unwrap();

        let meta = fs::symlink_metadata(&p_gir).unwrap();
        let oid = repo_gir
            .odb()
            .write_raw(ObjectKind::Blob, content.as_bytes())
            .unwrap();
        index.add_entry(IndexEntry::from_metadata(rel, oid, 0, &meta));
    }

    let gir_tree_oid = write_index_to_tree(&mut index, repo_gir.odb()).unwrap();
    index.write(repo_gir.git_dir()).unwrap();

    let st_add = Command::new("git")
        .args(["add", "."])
        .current_dir(dir_git.path())
        .status()
        .unwrap();
    assert!(st_add.success());

    let git_write_tree = Command::new("git")
        .args(["write-tree"])
        .current_dir(dir_git.path())
        .output()
        .unwrap();
    assert!(git_write_tree.status.success());
    let git_tree_hex = String::from_utf8_lossy(&git_write_tree.stdout)
        .trim()
        .to_string();

    assert_eq!(gir_tree_oid.to_hex(), git_tree_hex);

    // Also verify the `TREE` cache-tree extension bytes in `.git/index` match `/usr/bin/git`!
    let gir_idx_bytes = fs::read(dir_gir.path().join(".git/index")).unwrap();
    let git_idx_bytes = fs::read(dir_git.path().join(".git/index")).unwrap();
    let (_, _, gir_exts) = gir_core::index::parse_index_bytes(&gir_idx_bytes).unwrap();
    let (_, _, git_exts) = gir_core::index::parse_index_bytes(&git_idx_bytes).unwrap();
    let gir_tree_ext = gir_exts.iter().find(|e| e.is_cache_tree()).unwrap();
    let git_tree_ext = git_exts.iter().find(|e| e.is_cache_tree()).unwrap();
    assert_eq!(gir_tree_ext.data, git_tree_ext.data);

    // Verify official `/usr/bin/git fsck --full` succeeds on the `gir` repository
    let fsck = Command::new("git")
        .args(["fsck", "--full"])
        .current_dir(dir_gir.path())
        .output()
        .unwrap();
    assert!(
        fsck.status.success(),
        "git fsck failed: {}",
        String::from_utf8_lossy(&fsck.stderr)
    );
}

#[test]
fn commit_branch_tag_and_revwalk_differential() {
    let dir = TempDir::new().unwrap();
    let repo = Repository::init(dir.path(), false).unwrap();

    let mut cfg = repo.local_config().unwrap();
    cfg.set("user.name", "Irfan").unwrap();
    cfg.set("user.email", "irfan@example.com").unwrap();
    cfg.save_to(repo.git_dir().join("config")).unwrap();

    // Test unborn branch rename (`gir branch -M main`)
    rename_branch(&repo, None, "main", true).unwrap();
    assert_eq!(
        repo.refs().read_symbolic_ref("HEAD").unwrap(),
        "refs/heads/main"
    );

    // Stage and create root commit
    let file_a = dir.path().join("a.txt");
    fs::write(&file_a, "line 1\nline 2\n").unwrap();
    let meta_a = fs::symlink_metadata(&file_a).unwrap();
    let blob_a = repo
        .odb()
        .write_raw(ObjectKind::Blob, b"line 1\nline 2\n")
        .unwrap();
    let mut index = GitIndex::read(repo.git_dir()).unwrap();
    index.add_entry(IndexEntry::from_metadata("a.txt", blob_a, 0, &meta_a));
    index.write(repo.git_dir()).unwrap();

    let c1 = create_commit(
        &repo,
        &CommitOptions {
            message: "feat: root commit".to_string(),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(c1.is_root);
    assert_eq!(c1.diff_summary.files_changed(), 1);
    assert_eq!(c1.diff_summary.total_insertions, 2);

    // Modify `a.txt` and commit with `-a` (`all: true`)
    fs::write(&file_a, "line 1\nline 2 modified\nline 3\n").unwrap();
    let c2 = create_commit(
        &repo,
        &CommitOptions {
            message: "feat: second commit".to_string(),
            all: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!c2.is_root);
    assert_eq!(c2.diff_summary.total_insertions, 2);
    assert_eq!(c2.diff_summary.total_deletions, 1);

    // Create branch `feature` at `HEAD~1`, then rename it to `feat-x`
    let b_oid = create_branch(&repo, "feature", Some("HEAD~1"), false).unwrap();
    assert_eq!(b_oid, c1.commit_oid);
    rename_branch(&repo, Some("feature"), "feat-x", false).unwrap();

    let branches = list_branches(&repo, false, false).unwrap();
    let names: Vec<&str> = branches.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(names, vec!["feat-x", "main"]);

    // Safe delete `-d` succeeds because `feat-x` (`c1`) is an ancestor of `HEAD` (`c2`)
    delete_branch(&repo, "feat-x", false).unwrap();

    // Create lightweight and annotated tags
    create_lightweight_tag(&repo, "v0.1.0", Some(&c1.commit_oid.to_hex()), false).unwrap();
    create_annotated_tag(&repo, "v1.0.0", Some("HEAD"), "Release 1.0.0", false).unwrap();
    assert_eq!(
        list_tags(&repo, None).unwrap(),
        vec!["v0.1.0".to_string(), "v1.0.0".to_string()]
    );
    delete_tag(&repo, "v0.1.0").unwrap();
    assert_eq!(list_tags(&repo, None).unwrap(), vec!["v1.0.0".to_string()]);

    // Verify `RevWalk` range `c1..c2` yields only `[c2]`
    let range_spec = format!("{}..{}", c1.commit_oid, c2.commit_oid);
    let walk_items: Vec<_> = RevWalk::from_revisions(&repo, &[range_spec], false)
        .unwrap()
        .collect();
    assert_eq!(walk_items.len(), 1);
    assert_eq!(walk_items[0].oid, c2.commit_oid);

    // Test `--amend` changing commit message while keeping tree changes relative to `c1`
    let c2_amended = create_commit(
        &repo,
        &CommitOptions {
            message: "feat: second commit (amended)".to_string(),
            amend: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(c2_amended.subject, "feat: second commit (amended)");
    assert_ne!(c2_amended.commit_oid, c2.commit_oid);
    assert_eq!(
        repo.refs().resolve_to_oid("HEAD").unwrap(),
        c2_amended.commit_oid
    );

    // Verify empty commit without `--allow-empty` is rejected
    let empty_err = create_commit(
        &repo,
        &CommitOptions {
            message: "should fail".to_string(),
            ..Default::default()
        },
    );
    assert!(empty_err.is_err());

    // Verify `--allow-empty` succeeds
    let c3_empty = create_commit(
        &repo,
        &CommitOptions {
            message: "chore: empty trigger".to_string(),
            allow_empty: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(c3_empty.diff_summary.files_changed(), 0);
}
