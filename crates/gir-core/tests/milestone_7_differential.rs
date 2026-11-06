use std::collections::HashSet;
use std::fs;
use std::process::Command;

use gir_core::branch_ops::rename_branch;
use gir_core::commit_ops::{create_commit, CommitOptions};
use gir_core::index::{GitIndex, IndexEntry};
use gir_core::merge::{find_merge_bases, merge_branch, MergeOptions, MergeStatus};
use gir_core::object::ObjectKind;
use gir_core::rebase::{
    cherry_pick, run_rebase, CherryPickOptions, CherryPickStatus, RebaseOptions, RebaseStatus,
};
use gir_core::repository::Repository;
use gir_core::worktree::{switch_or_checkout, SwitchOptions};
use tempfile::TempDir;

fn stage_all_and_commit(repo: &Repository, msg: &str) -> gir_core::ObjectId {
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
    .unwrap()
    .commit_oid
}

#[test]
fn merge_base_diamond_and_criss_cross_differential_with_official_git() {
    let dir = TempDir::new().unwrap();
    let repo = Repository::init(dir.path(), false).unwrap();
    let mut cfg = repo.local_config().unwrap();
    cfg.set("user.name", "Irfan").unwrap();
    cfg.set("user.email", "irfan@example.com").unwrap();
    cfg.save_to(repo.git_dir().join("config")).unwrap();
    rename_branch(&repo, None, "main", true).unwrap();

    fs::write(dir.path().join("f.txt"), "root\n").unwrap();
    let r = stage_all_and_commit(&repo, "root");

    // Branch A1 and B1 from R
    switch_or_checkout(
        &repo,
        &SwitchOptions {
            create_branch: Some("b-branch".to_string()),
            ..Default::default()
        },
    )
    .unwrap();
    fs::write(dir.path().join("b.txt"), "b1\n").unwrap();
    let b1 = stage_all_and_commit(&repo, "B1");

    switch_or_checkout(
        &repo,
        &SwitchOptions {
            target: Some("main".to_string()),
            ..Default::default()
        },
    )
    .unwrap();
    fs::write(dir.path().join("a.txt"), "a1\n").unwrap();
    let a1 = stage_all_and_commit(&repo, "A1");

    // Compare `find_merge_bases(A1, B1)` against `/usr/bin/git merge-base --all`
    let gir_bases = find_merge_bases(repo.odb(), &a1, &b1).unwrap();
    assert_eq!(gir_bases, vec![r]);

    let git_out = Command::new("git")
        .args(["merge-base", "--all", &a1.to_hex(), &b1.to_hex()])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(git_out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&git_out.stdout).trim(),
        r.to_hex()
    );

    // Build criss-cross merge: A2 merges B1 into A1; B2 merges A1 into B1
    let merge_a2 = merge_branch(
        &repo,
        &MergeOptions {
            target: Some("b-branch".to_string()),
            message: Some("A2 merge".to_string()),
            ..Default::default()
        },
    )
    .unwrap();
    let MergeStatus::Merged { commit_oid: a2, .. } = merge_a2 else {
        panic!("expected clean merge for A2");
    };

    switch_or_checkout(
        &repo,
        &SwitchOptions {
            target: Some("b-branch".to_string()),
            ..Default::default()
        },
    )
    .unwrap();
    let merge_b2 = merge_branch(
        &repo,
        &MergeOptions {
            target: Some(a1.to_hex()),
            message: Some("B2 merge".to_string()),
            ..Default::default()
        },
    )
    .unwrap();
    let MergeStatus::Merged { commit_oid: b2, .. } = merge_b2 else {
        panic!("expected clean merge for B2");
    };

    let gir_criss: HashSet<String> = find_merge_bases(repo.odb(), &a2, &b2)
        .unwrap()
        .into_iter()
        .map(|id| id.to_hex())
        .collect();

    let git_criss_out = Command::new("git")
        .args(["merge-base", "--all", &a2.to_hex(), &b2.to_hex()])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let git_criss: HashSet<String> = String::from_utf8_lossy(&git_criss_out.stdout)
        .lines()
        .map(|s| s.trim().to_string())
        .collect();

    assert_eq!(gir_criss, git_criss);
    assert_eq!(gir_criss.len(), 2);
}

#[test]
fn non_overlapping_same_file_3way_merge_and_conflicted_merge_reuc_interop() {
    let dir = TempDir::new().unwrap();
    let repo = Repository::init(dir.path(), false).unwrap();
    let mut cfg = repo.local_config().unwrap();
    cfg.set("user.name", "Irfan").unwrap();
    cfg.set("user.email", "irfan@example.com").unwrap();
    cfg.save_to(repo.git_dir().join("config")).unwrap();
    rename_branch(&repo, None, "main", true).unwrap();

    let base_lines: String = (1..=45).map(|i| format!("line {i} original\n")).collect();
    fs::write(dir.path().join("app.rs"), &base_lines).unwrap();
    stage_all_and_commit(&repo, "c1: base app.rs");

    // Branch `feature` edits line 40
    switch_or_checkout(
        &repo,
        &SwitchOptions {
            create_branch: Some("feature".to_string()),
            ..Default::default()
        },
    )
    .unwrap();
    let mut feat_lines: Vec<String> = (1..=45).map(|i| format!("line {i} original")).collect();
    feat_lines[39] = "line 40 EDITED BY FEATURE".to_string();
    fs::write(dir.path().join("app.rs"), format!("{}\n", feat_lines.join("\n"))).unwrap();
    stage_all_and_commit(&repo, "c2: feature edits line 40");

    // Switch back to `main` and edit line 2
    switch_or_checkout(
        &repo,
        &SwitchOptions {
            target: Some("main".to_string()),
            ..Default::default()
        },
    )
    .unwrap();
    let mut main_lines: Vec<String> = (1..=45).map(|i| format!("line {i} original")).collect();
    main_lines[1] = "line 2 EDITED BY MAIN".to_string();
    fs::write(dir.path().join("app.rs"), format!("{}\n", main_lines.join("\n"))).unwrap();
    stage_all_and_commit(&repo, "c3: main edits line 2");

    // 1. Merge `feature` into `main` -> should cleanly auto-merge both edits in `app.rs`!
    let status = merge_branch(
        &repo,
        &MergeOptions {
            target: Some("feature".to_string()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(matches!(status, MergeStatus::Merged { .. }));

    let merged_text = fs::read_to_string(dir.path().join("app.rs")).unwrap();
    assert!(merged_text.contains("line 2 EDITED BY MAIN\n"));
    assert!(merged_text.contains("line 40 EDITED BY FEATURE\n"));

    assert!(Command::new("git")
        .args(["fsck", "--full"])
        .current_dir(dir.path())
        .status()
        .unwrap()
        .success());

    // 2. Now create a conflicting edit on line 10 of `app.rs` on both branches
    switch_or_checkout(
        &repo,
        &SwitchOptions {
            target: Some("feature".to_string()),
            ..Default::default()
        },
    )
    .unwrap();
    feat_lines[9] = "line 10 FEATURE CONFLICT".to_string();
    fs::write(dir.path().join("app.rs"), format!("{}\n", feat_lines.join("\n"))).unwrap();
    stage_all_and_commit(&repo, "c4: feature edits line 10");

    switch_or_checkout(
        &repo,
        &SwitchOptions {
            target: Some("main".to_string()),
            ..Default::default()
        },
    )
    .unwrap();
    let mut current_main: Vec<String> = merged_text.lines().map(ToOwned::to_owned).collect();
    current_main[9] = "line 10 MAIN CONFLICT".to_string();
    fs::write(dir.path().join("app.rs"), format!("{}\n", current_main.join("\n"))).unwrap();
    stage_all_and_commit(&repo, "c5: main edits line 10");

    let conflict_status = merge_branch(
        &repo,
        &MergeOptions {
            target: Some("feature".to_string()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(matches!(conflict_status, MergeStatus::Conflicted { .. }));

    let conflicted_file = fs::read_to_string(dir.path().join("app.rs")).unwrap();
    assert!(conflicted_file.contains("<<<<<<< HEAD\nline 10 MAIN CONFLICT\n=======\nline 10 FEATURE CONFLICT\n>>>>>>> feature\n"));

    // Verify official `/usr/bin/git status --porcelain` sees `UU app.rs` (stages 1, 2, 3)
    let git_status_out = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&git_status_out.stdout),
        "UU app.rs\n"
    );

    // Resolve the conflict, stage `app.rs`, and verify `REUC` extension is populated
    current_main[9] = "line 10 RESOLVED TOGETHER".to_string();
    let resolved_bytes = format!("{}\n", current_main.join("\n")).into_bytes();
    fs::write(dir.path().join("app.rs"), &resolved_bytes).unwrap();
    let resolved_oid = repo.odb().write_raw(ObjectKind::Blob, &resolved_bytes).unwrap();
    let meta = fs::symlink_metadata(dir.path().join("app.rs")).unwrap();

    let mut index = GitIndex::read(repo.git_dir()).unwrap();
    assert!(index.has_conflicts());
    index.add_entry(IndexEntry::from_metadata("app.rs", resolved_oid, 0, &meta));
    assert!(!index.has_conflicts());
    assert!(index.extensions().iter().any(|ext| ext.is_resolve_undo()));
    index.write(repo.git_dir()).unwrap();

    // Conclude merge via `merge_branch --continue`
    let cont_status = merge_branch(
        &repo,
        &MergeOptions {
            cont: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(matches!(cont_status, MergeStatus::Merged { .. }));
    assert!(!repo.git_dir().join("MERGE_HEAD").exists());
}

#[test]
fn cherry_pick_and_rebase_sequencer_workflow() {
    let dir = TempDir::new().unwrap();
    let repo = Repository::init(dir.path(), false).unwrap();
    let mut cfg = repo.local_config().unwrap();
    cfg.set("user.name", "Irfan").unwrap();
    cfg.set("user.email", "irfan@example.com").unwrap();
    cfg.save_to(repo.git_dir().join("config")).unwrap();
    rename_branch(&repo, None, "main", true).unwrap();

    fs::write(dir.path().join("base.txt"), "base\n").unwrap();
    stage_all_and_commit(&repo, "c1: base");

    // Create `topic` branch with 2 commits (`t1.txt`, `t2.txt`)
    switch_or_checkout(
        &repo,
        &SwitchOptions {
            create_branch: Some("topic".to_string()),
            ..Default::default()
        },
    )
    .unwrap();
    fs::write(dir.path().join("t1.txt"), "topic 1\n").unwrap();
    let t1_oid = stage_all_and_commit(&repo, "topic: add t1");

    fs::write(dir.path().join("t2.txt"), "topic 2\n").unwrap();
    let _t2_oid = stage_all_and_commit(&repo, "topic: add t2");

    // Advance `main` with `m1.txt` and cherry-pick `t1_oid`
    switch_or_checkout(
        &repo,
        &SwitchOptions {
            target: Some("main".to_string()),
            ..Default::default()
        },
    )
    .unwrap();
    fs::write(dir.path().join("m1.txt"), "main 1\n").unwrap();
    stage_all_and_commit(&repo, "main: add m1");

    let cp_res = cherry_pick(
        &repo,
        &CherryPickOptions {
            commits: vec![t1_oid.to_hex()],
            ..Default::default()
        },
    )
    .unwrap();
    assert!(matches!(cp_res, CherryPickStatus::Applied { .. }));
    assert_eq!(fs::read_to_string(dir.path().join("t1.txt")).unwrap(), "topic 1\n");

    // Switch back to `topic` and rebase `topic` onto `main`
    switch_or_checkout(
        &repo,
        &SwitchOptions {
            target: Some("topic".to_string()),
            ..Default::default()
        },
    )
    .unwrap();
    let rb_res = run_rebase(
        &repo,
        &RebaseOptions {
            upstream: Some("main".to_string()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(matches!(rb_res, RebaseStatus::Completed { .. }));
    assert!(dir.path().join("m1.txt").is_file());
    assert!(dir.path().join("t1.txt").is_file());
    assert!(dir.path().join("t2.txt").is_file());

    assert!(Command::new("git")
        .args(["fsck", "--full"])
        .current_dir(dir.path())
        .status()
        .unwrap()
        .success());
}

#[test]
fn modify_delete_and_add_add_conflict_status_differential() {
    let dir = TempDir::new().unwrap();
    let repo = Repository::init(dir.path(), false).unwrap();
    let mut cfg = repo.local_config().unwrap();
    cfg.set("user.name", "Irfan").unwrap();
    cfg.set("user.email", "irfan@example.com").unwrap();
    cfg.save_to(repo.git_dir().join("config")).unwrap();
    rename_branch(&repo, None, "main", true).unwrap();

    fs::write(dir.path().join("mod_del.txt"), "base line\n").unwrap();
    stage_all_and_commit(&repo, "c1: base");

    // On `other` branch: delete `mod_del.txt` and add `both_added.txt` ("other version")
    switch_or_checkout(
        &repo,
        &SwitchOptions {
            create_branch: Some("other".to_string()),
            ..Default::default()
        },
    )
    .unwrap();
    fs::remove_file(dir.path().join("mod_del.txt")).unwrap();
    let mut idx = GitIndex::read(repo.git_dir()).unwrap();
    idx.remove_path("mod_del.txt");
    idx.write(repo.git_dir()).unwrap();
    fs::write(dir.path().join("both_added.txt"), "added on other\n").unwrap();
    stage_all_and_commit(&repo, "c2: other deletes mod_del and adds both_added");

    // On `main` branch: modify `mod_del.txt` and add `both_added.txt` ("main version")
    switch_or_checkout(
        &repo,
        &SwitchOptions {
            target: Some("main".to_string()),
            ..Default::default()
        },
    )
    .unwrap();
    fs::write(dir.path().join("mod_del.txt"), "modified on main\n").unwrap();
    fs::write(dir.path().join("both_added.txt"), "added on main\n").unwrap();
    stage_all_and_commit(&repo, "c3: main modifies mod_del and adds both_added");

    // Merge `other` into `main` -> triggers both `AA both_added.txt` and `UD mod_del.txt`!
    let status = merge_branch(
        &repo,
        &MergeOptions {
            target: Some("other".to_string()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(matches!(status, MergeStatus::Conflicted { .. }));

    let gir_porcelain = gir_core::status::compute_status(&repo, Default::default())
        .unwrap()
        .format_porcelain(false);
    let git_out = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let git_porcelain = String::from_utf8_lossy(&git_out.stdout);
    assert_eq!(gir_porcelain, git_porcelain);
    assert_eq!(gir_porcelain, "AA both_added.txt\nUD mod_del.txt\n");

    // Abort merge and verify clean state
    let abort_status = merge_branch(
        &repo,
        &MergeOptions {
            abort: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(matches!(abort_status, MergeStatus::Aborted));
    assert!(gir_core::status::compute_status(&repo, Default::default())
        .unwrap()
        .is_clean());
}
