use std::fs;
use std::process::Command;

use gir_core::branch_ops::rename_branch;
use gir_core::commit_ops::{create_commit, CommitOptions};
use gir_core::diff::diff_index_to_worktree;
use gir_core::index::{GitIndex, IndexEntry};
use gir_core::object::ObjectKind;
use gir_core::repository::Repository;
use gir_core::stash::{stash_list, stash_pop, stash_push};
use gir_core::worktree::{
    clean_worktree, reset_head, restore_paths, switch_or_checkout, CleanOptions, ResetMode,
    RestoreOptions, SwitchOptions,
};
use tempfile::TempDir;

fn stage_all_and_commit(repo: &Repository, msg: &str) {
    let wt = repo.worktree().unwrap();
    let mut index = GitIndex::read(repo.git_dir()).unwrap();
    for entry in fs::read_dir(wt).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == ".git" {
            continue;
        }
        let path = entry.path();
        let meta = fs::symlink_metadata(&path).unwrap();
        if meta.is_file() {
            let data = fs::read(&path).unwrap();
            let oid = repo.odb().write_raw(ObjectKind::Blob, &data).unwrap();
            index.add_entry(IndexEntry::from_metadata(&name, oid, 0, &meta));
        }
    }
    index.write(repo.git_dir()).unwrap();
    create_commit(
        repo,
        &CommitOptions {
            message: msg.to_string(),
            ..Default::default()
        },
    )
    .unwrap();
}

#[test]
fn unified_diff_patch_is_accepted_by_official_git_apply() {
    let dir_src = TempDir::new().unwrap();
    let dir_dst = TempDir::new().unwrap();

    let repo_src = Repository::init(dir_src.path(), false).unwrap();
    let mut cfg = repo_src.local_config().unwrap();
    cfg.set("user.name", "Irfan").unwrap();
    cfg.set("user.email", "irfan@example.com").unwrap();
    cfg.save_to(repo_src.git_dir().join("config")).unwrap();

    let initial: String = (1..=30).map(|i| format!("line {i} original\n")).collect();
    fs::write(dir_src.path().join("code.rs"), &initial).unwrap();
    fs::write(dir_dst.path().join("code.rs"), &initial).unwrap();

    stage_all_and_commit(&repo_src, "feat: initial");

    // Initialize `dir_dst` with official `git` so `git apply` can check & apply our patch
    assert!(Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir_dst.path())
        .status()
        .unwrap()
        .success());

    // Make two separated hunk edits in `dir_src/code.rs`
    let mut lines: Vec<String> = (1..=30).map(|i| format!("line {i} original")).collect();
    lines[2] = "line 3 REPLACED IN HUNK 1".to_string();
    lines.insert(3, "line 3b INSERTED IN HUNK 1".to_string());
    lines[26] = "line 26 REPLACED IN HUNK 2".to_string();
    let modified = format!("{}\n", lines.join("\n"));
    fs::write(dir_src.path().join("code.rs"), &modified).unwrap();

    let index = GitIndex::read(repo_src.git_dir()).unwrap();
    let patches = diff_index_to_worktree(&repo_src, &index, &[]).unwrap();
    assert_eq!(patches.len(), 1);
    assert_eq!(patches[0].hunks.len(), 2);

    let patch_text = patches[0].format_unified();
    let patch_file = dir_dst.path().join("gir.patch");
    fs::write(&patch_file, &patch_text).unwrap();

    // Verify `/usr/bin/git apply --check gir.patch` and `/usr/bin/git apply gir.patch` succeed!
    let check_out = Command::new("git")
        .args(["apply", "--check", "gir.patch"])
        .current_dir(dir_dst.path())
        .output()
        .unwrap();
    assert!(
        check_out.status.success(),
        "git apply --check failed: {}",
        String::from_utf8_lossy(&check_out.stderr)
    );

    assert!(Command::new("git")
        .args(["apply", "gir.patch"])
        .current_dir(dir_dst.path())
        .status()
        .unwrap()
        .success());

    let applied_content = fs::read_to_string(dir_dst.path().join("code.rs")).unwrap();
    assert_eq!(applied_content, modified);
}

#[test]
fn two_tree_safe_checkout_matrix_and_carry_over() {
    let dir = TempDir::new().unwrap();
    let repo = Repository::init(dir.path(), false).unwrap();
    let mut cfg = repo.local_config().unwrap();
    cfg.set("user.name", "Irfan").unwrap();
    cfg.set("user.email", "irfan@example.com").unwrap();
    cfg.save_to(repo.git_dir().join("config")).unwrap();
    rename_branch(&repo, None, "main", true).unwrap();

    // Commit `a.txt` and `b.txt` on `main`
    fs::write(dir.path().join("a.txt"), "a v1\n").unwrap();
    fs::write(dir.path().join("b.txt"), "b v1\n").unwrap();
    stage_all_and_commit(&repo, "c1: base");

    // Switch to new branch `feature` and modify `b.txt` + add `only_feat.txt`
    switch_or_checkout(
        &repo,
        &SwitchOptions {
            create_branch: Some("feature".to_string()),
            ..Default::default()
        },
    )
    .unwrap();
    fs::write(dir.path().join("b.txt"), "b feature v2\n").unwrap();
    fs::write(dir.path().join("only_feat.txt"), "feat file\n").unwrap();
    stage_all_and_commit(&repo, "c2: feature changes");

    // Switch back to `main` (`only_feat.txt` is removed, `b.txt` is back to `b v1\n`)
    switch_or_checkout(
        &repo,
        &SwitchOptions {
            target: Some("main".to_string()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!dir.path().join("only_feat.txt").exists());
    assert_eq!(fs::read_to_string(dir.path().join("b.txt")).unwrap(), "b v1\n");

    // Dirty both `a.txt` (identical between `main` and `feature`) and `b.txt` (differs between `main` and `feature`)
    fs::write(dir.path().join("a.txt"), "a dirty carry-over\n").unwrap();
    fs::write(dir.path().join("b.txt"), "b local dirty conflict\n").unwrap();

    // Switching to `feature` MUST abort because `b.txt` would be overwritten!
    let err = switch_or_checkout(
        &repo,
        &SwitchOptions {
            target: Some("feature".to_string()),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(err.to_string().contains("b.txt"));
    // Both dirty files must still be completely untouched on disk
    assert_eq!(
        fs::read_to_string(dir.path().join("a.txt")).unwrap(),
        "a dirty carry-over\n"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("b.txt")).unwrap(),
        "b local dirty conflict\n"
    );

    // Restore `b.txt` using `restore_paths`, then create untracked `only_feat.txt` to test untracked overwrite protection
    restore_paths(
        &repo,
        &RestoreOptions {
            pathspecs: vec!["b.txt".to_string()],
            ..Default::default()
        },
    )
    .unwrap();
    fs::write(dir.path().join("only_feat.txt"), "untracked collision\n").unwrap();

    let err_untracked = switch_or_checkout(
        &repo,
        &SwitchOptions {
            target: Some("feature".to_string()),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(err_untracked.to_string().contains("only_feat.txt"));

    // Clean untracked file via `clean_worktree`, then switch to `feature` and verify `a.txt` carried over!
    clean_worktree(
        &repo,
        &CleanOptions {
            force: true,
            ..Default::default()
        },
    )
    .unwrap();

    switch_or_checkout(
        &repo,
        &SwitchOptions {
            target: Some("feature".to_string()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(dir.path().join("a.txt")).unwrap(),
        "a dirty carry-over\n"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("b.txt")).unwrap(),
        "b feature v2\n"
    );

    // Test `reset_head` (`ResetMode::Hard`) and `stash_push` / `stash_pop`
    stash_push(&repo, Some("saving carry-over"), false).unwrap();
    assert_eq!(fs::read_to_string(dir.path().join("a.txt")).unwrap(), "a v1\n");
    assert_eq!(stash_list(&repo).unwrap().len(), 1);

    stash_pop(&repo, None, false).unwrap();
    assert_eq!(
        fs::read_to_string(dir.path().join("a.txt")).unwrap(),
        "a dirty carry-over\n"
    );
    assert_eq!(stash_list(&repo).unwrap().len(), 0);

    reset_head(&repo, "HEAD~1", ResetMode::Hard).unwrap();
    assert_eq!(fs::read_to_string(dir.path().join("a.txt")).unwrap(), "a v1\n");
    assert_eq!(fs::read_to_string(dir.path().join("b.txt")).unwrap(), "b v1\n");
}

#[test]
fn df_checkout_collisions_and_atomic_stash_pop_rollback() {
    let dir = TempDir::new().unwrap();
    let repo = Repository::init(dir.path(), false).unwrap();
    let mut cfg = repo.local_config().unwrap();
    cfg.set("user.name", "Irfan").unwrap();
    cfg.set("user.email", "irfan@example.com").unwrap();
    cfg.save_to(repo.git_dir().join("config")).unwrap();
    rename_branch(&repo, None, "main", true).unwrap();

    // Commit `base.txt` on `main`
    fs::write(dir.path().join("base.txt"), "line 1\nline 2\n").unwrap();
    stage_all_and_commit(&repo, "c1: base");

    // Create branch `dir-branch` with `sub/nested.txt`
    switch_or_checkout(
        &repo,
        &SwitchOptions {
            create_branch: Some("dir-branch".to_string()),
            ..Default::default()
        },
    )
    .unwrap();
    fs::create_dir_all(dir.path().join("sub")).unwrap();
    let nested_bytes = b"nested content\n";
    fs::write(dir.path().join("sub/nested.txt"), nested_bytes).unwrap();
    let mut index = GitIndex::read(repo.git_dir()).unwrap();
    let nested_oid = repo.odb().write_raw(ObjectKind::Blob, nested_bytes).unwrap();
    let nested_meta = fs::symlink_metadata(dir.path().join("sub/nested.txt")).unwrap();
    index.add_entry(IndexEntry::from_metadata(
        "sub/nested.txt",
        nested_oid,
        0,
        &nested_meta,
    ));
    index.write(repo.git_dir()).unwrap();
    create_commit(
        &repo,
        &CommitOptions {
            message: "c2: add sub/nested.txt".to_string(),
            ..Default::default()
        },
    )
    .unwrap();

    // Switch back to `main` (which prunes `sub/nested.txt` and empty `sub/`)
    switch_or_checkout(
        &repo,
        &SwitchOptions {
            target: Some("main".to_string()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!dir.path().join("sub").exists());

    // Create an untracked regular file `sub` on `main` -> switching to `dir-branch` must abort!
    fs::write(dir.path().join("sub"), "untracked file blocking dir\n").unwrap();
    let err = switch_or_checkout(
        &repo,
        &SwitchOptions {
            target: Some("dir-branch".to_string()),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(err.to_string().contains("sub"));
    assert_eq!(
        fs::read_to_string(dir.path().join("sub")).unwrap(),
        "untracked file blocking dir\n"
    );

    // Remove untracked `sub`, switch to `dir-branch` cleanly
    fs::remove_file(dir.path().join("sub")).unwrap();
    switch_or_checkout(
        &repo,
        &SwitchOptions {
            target: Some("dir-branch".to_string()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(dir.path().join("sub/nested.txt").is_file());

    // Test atomic `stash_pop` rollback when an untracked file from P3 collides on disk
    fs::write(dir.path().join("base.txt"), "line 1 stashed\nline 2\n").unwrap();
    fs::write(dir.path().join("extra.txt"), "stashed untracked\n").unwrap();
    stash_push(&repo, Some("with untracked"), true).unwrap();
    assert!(!dir.path().join("extra.txt").exists());
    assert_eq!(
        fs::read_to_string(dir.path().join("base.txt")).unwrap(),
        "line 1\nline 2\n"
    );

    // Create conflicting `extra.txt` on disk before `stash_pop`:
    // `stash_pop` must abort in Phase 1 BEFORE modifying `base.txt` or dropping `stash@{0}`!
    fs::write(dir.path().join("extra.txt"), "on-disk collision\n").unwrap();
    assert!(stash_pop(&repo, None, false).is_err());
    assert_eq!(
        fs::read_to_string(dir.path().join("base.txt")).unwrap(),
        "line 1\nline 2\n"
    );
    assert_eq!(stash_list(&repo).unwrap().len(), 1);
}
