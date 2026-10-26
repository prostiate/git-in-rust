use std::fs;
use std::process::Command;

use gir_core::object::commit::Signature;
use gir_core::reflog::snapshot::SafetySnapshotEngine;
use gir_core::revision::parse_revision;
use gir_core::{GirError, ObjectId, Repository};
use tempfile::TempDir;

fn run_git(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    let out = Command::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Irfan")
        .env("GIT_AUTHOR_EMAIL", "irfan@example.com")
        .env("GIT_COMMITTER_NAME", "Irfan")
        .env("GIT_COMMITTER_EMAIL", "irfan@example.com")
        .args(args)
        .output()
        .expect("failed to execute official git");
    assert!(
        out.status.success(),
        "official `git {:?}` failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn git_rev_parse(dir: &std::path::Path, spec: &str) -> ObjectId {
    let out = run_git(dir, &["rev-parse", spec]);
    let hex = String::from_utf8(out.stdout).unwrap();
    ObjectId::from_hex(hex.trim()).unwrap()
}

#[test]
fn rev_parse_torture_differential() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    run_git(root, &["init", "-b", "main"]);
    fs::create_dir_all(root.join("src/core")).unwrap();

    // Commit 1 on main
    fs::write(root.join("src/core/engine.rs"), "pub fn v1() {}\n").unwrap();
    run_git(root, &["add", "."]);
    run_git(root, &["commit", "-m", "c1"]);

    // Commit 2 on main
    fs::write(root.join("src/core/engine.rs"), "pub fn v2() {}\n").unwrap();
    run_git(root, &["add", "."]);
    run_git(root, &["commit", "-m", "c2"]);

    // Create feature branch and make 2 commits on feature
    run_git(root, &["checkout", "-b", "feature"]);
    fs::write(root.join("src/core/feature.rs"), "pub fn feat1() {}\n").unwrap();
    run_git(root, &["add", "."]);
    run_git(root, &["commit", "-m", "f1"]);

    fs::write(root.join("src/core/feature.rs"), "pub fn feat2() {}\n").unwrap();
    run_git(root, &["add", "."]);
    run_git(root, &["commit", "-m", "f2"]);

    // Switch back to main, make a commit, and merge feature (--no-ff to create 2-parent merge commit)
    run_git(root, &["checkout", "main"]);
    fs::write(root.join("README.md"), "# Main branch\n").unwrap();
    run_git(root, &["add", "."]);
    run_git(root, &["commit", "-m", "c3"]);
    run_git(root, &["merge", "--no-ff", "feature", "-m", "merge feature"]);

    // Create annotated tag v1.0 on merge commit
    run_git(root, &["tag", "-a", "v1.0", "-m", "Release 1.0"]);

    let repo = Repository::discover(root).unwrap();

    let specs = [
        "HEAD",
        "@",
        "main",
        "feature",
        "v1.0",
        "HEAD~",
        "HEAD~1",
        "HEAD~2",
        "HEAD^",
        "HEAD^1",
        "HEAD^2",
        "HEAD^2~1",
        "HEAD~1^1",
        "v1.0^0",
        "v1.0^{}",
        "v1.0^{commit}",
        "v1.0^{tree}",
        "HEAD^{tree}",
        "HEAD:src/core/engine.rs",
        "HEAD:src/core",
        "HEAD@{0}",
        "HEAD@{1}",
        "HEAD@{2}",
        "@{-1}",
        "main@{0}",
        "main@{1}",
    ];

    for spec in &specs {
        let expected = git_rev_parse(root, spec);
        let actual = parse_revision(&repo, spec).unwrap_or_else(|e| {
            panic!("gir failed to parse revision '{spec}': {e}");
        });
        assert_eq!(
            actual, expected,
            "revision mismatch for '{spec}': gir={actual}, git={expected}"
        );
    }

    // Now pack refs using official git and verify all revision expressions STILL resolve identically
    run_git(root, &["pack-refs", "--all"]);
    for spec in &specs {
        let expected = git_rev_parse(root, spec);
        let actual = parse_revision(&repo, spec).unwrap();
        assert_eq!(actual, expected, "post-pack-refs mismatch for '{spec}'");
    }
}

#[test]
fn update_ref_cas_and_reflog_interoperability() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    run_git(root, &["init", "-b", "main"]);
    fs::write(root.join("a.txt"), "v1\n").unwrap();
    run_git(root, &["add", "."]);
    run_git(root, &["commit", "-m", "c1"]);
    let c1 = git_rev_parse(root, "HEAD");

    fs::write(root.join("a.txt"), "v2\n").unwrap();
    run_git(root, &["add", "."]);
    run_git(root, &["commit", "-m", "c2"]);
    let c2 = git_rev_parse(root, "HEAD");

    let repo = Repository::discover(root).unwrap();
    let sig = Signature::new("Irfan", "irfan@example.com", 1_760_000_000, "+0700");

    // 1. CAS mismatch must fail without mutating the ref
    let bad_cas = repo.refs().update_ref(
        "refs/heads/main",
        c1,
        Some(c1), // wrong expected old (actual is c2)
        false,
        &sig,
        "reset: moving to c1",
    );
    assert!(matches!(bad_cas, Err(GirError::RefCasMismatch { .. })));
    assert_eq!(repo.refs().resolve_to_oid("refs/heads/main").unwrap(), c2);

    // 2. Valid CAS update from c2 -> c1
    repo.refs()
        .update_ref(
            "refs/heads/main",
            c1,
            Some(c2),
            false,
            &sig,
            "reset: moving to c1",
        )
        .unwrap();
    assert_eq!(repo.refs().resolve_to_oid("HEAD").unwrap(), c1);

    // 3. Official `/usr/bin/git reflog show` and `git rev-parse HEAD@{0}` / `HEAD@{1}` must read our reflog
    assert_eq!(git_rev_parse(root, "HEAD@{0}"), c1);
    assert_eq!(git_rev_parse(root, "HEAD@{1}"), c2);
    let reflog_out = run_git(root, &["reflog", "show", "main"]);
    let reflog_str = String::from_utf8(reflog_out.stdout).unwrap();
    assert!(reflog_str.contains("reset: moving to c1"));
}

#[test]
fn safety_snapshot_and_gir_undo_zero_data_loss() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    run_git(root, &["init", "-b", "main"]);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/main.rs"), "fn main() { println!(\"v1\"); }\n").unwrap();
    run_git(root, &["add", "."]);
    run_git(root, &["commit", "-m", "initial"]);

    // Modify tracked file AND create uncommitted new files
    fs::write(
        root.join("src/main.rs"),
        "fn main() { println!(\"dirty precious work\"); }\n",
    )
    .unwrap();
    fs::write(
        root.join("src/untracked.rs"),
        "pub const SECRET: u32 = 42;\n",
    )
    .unwrap();

    let repo = Repository::discover(root).unwrap();
    let snap = SafetySnapshotEngine::capture(&repo, "reset --hard HEAD").unwrap();

    // Official `git fsck --full --strict` must validate the snapshot objects under `refs/gir/snapshots/latest`
    run_git(root, &["fsck", "--full", "--strict"]);

    // Simulate a destructive catastrophe: wipe `src/untracked.rs`, overwrite `src/main.rs`, add junk file
    fs::remove_file(root.join("src/untracked.rs")).unwrap();
    fs::write(root.join("src/main.rs"), "corrupted\n").unwrap();
    fs::write(root.join("accident.tmp"), "junk\n").unwrap();

    // Run `SafetySnapshotEngine::restore` (`gir undo`)
    let undo_report = SafetySnapshotEngine::restore(&repo, None).unwrap();
    assert_eq!(undo_report.restored_snapshot, snap.snapshot_oid);
    assert_eq!(undo_report.operation, "reset --hard HEAD");

    // Verify every single file is restored byte-for-byte and junk file is cleaned up
    assert_eq!(
        fs::read_to_string(root.join("src/main.rs")).unwrap(),
        "fn main() { println!(\"dirty precious work\"); }\n"
    );
    assert_eq!(
        fs::read_to_string(root.join("src/untracked.rs")).unwrap(),
        "pub const SECRET: u32 = 42;\n"
    );
    assert!(!root.join("accident.tmp").exists());
}
