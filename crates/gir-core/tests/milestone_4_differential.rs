use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use gir_core::index::{GitIndex, IndexEntry};
use gir_core::status::{compute_status, StatusOptions};
use gir_core::{ObjectKind, Repository};
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

#[test]
fn index_dirc_interoperability_with_official_git() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    let repo = Repository::init(root, false).unwrap();
    let mut index = GitIndex::new();

    // Create 15 nested files of varying name lengths (to test all 1..=8 byte NUL padding alignments),
    // plus an executable script and a symlink
    fs::create_dir_all(root.join("src/nested/deep")).unwrap();
    for i in 0..15 {
        let rel = format!("src/nested/deep/module_{}_{}.rs", i, "x".repeat(i));
        let abs = root.join(&rel);
        let content = format!("pub const VAL_{i}: usize = {i};\n");
        fs::write(&abs, &content).unwrap();
        let meta = fs::symlink_metadata(&abs).unwrap();
        let oid = repo
            .odb()
            .write_raw(ObjectKind::Blob, content.as_bytes())
            .unwrap();
        index.add_entry(IndexEntry::from_metadata(rel, oid, 0, &meta));
    }

    // Add executable script (`100755`)
    let script_rel = "scripts/deploy.sh";
    fs::create_dir_all(root.join("scripts")).unwrap();
    let script_abs = root.join(script_rel);
    fs::write(&script_abs, "#!/bin/sh\necho deploy\n").unwrap();
    let mut perms = fs::metadata(&script_abs).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_abs, perms).unwrap();
    let script_meta = fs::symlink_metadata(&script_abs).unwrap();
    let script_oid = repo
        .odb()
        .write_raw(ObjectKind::Blob, b"#!/bin/sh\necho deploy\n")
        .unwrap();
    index.add_entry(IndexEntry::from_metadata(
        script_rel,
        script_oid,
        0,
        &script_meta,
    ));

    // Add symbolic link (`120000`)
    let link_rel = "scripts/latest.sh";
    let link_abs = root.join(link_rel);
    std::os::unix::fs::symlink("deploy.sh", &link_abs).unwrap();
    let link_meta = fs::symlink_metadata(&link_abs).unwrap();
    let link_oid = repo.odb().write_raw(ObjectKind::Blob, b"deploy.sh").unwrap();
    index.add_entry(IndexEntry::from_metadata(
        link_rel, link_oid, 0, &link_meta,
    ));

    index.write(repo.git_dir()).unwrap();

    // Official `/usr/bin/git status --porcelain=v1` must see every file staged as `A ` with ZERO unstaged diffs
    let git_status = run_git(root, &["status", "--porcelain=v1"]);
    let status_str = String::from_utf8(git_status.stdout).unwrap();
    for line in status_str.lines() {
        assert!(
            line.starts_with("A  "),
            "expected 'A  <path>' (cleanly staged, no unstaged diff), got '{line}'"
        );
    }
    assert_eq!(status_str.lines().count(), 17);
}

#[test]
fn cache_tree_extension_invalidation_on_gir_index_update() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    run_git(root, &["init", "-b", "main"]);
    fs::create_dir_all(root.join("a/b/c")).unwrap();
    fs::write(root.join("a/b/c/deep.txt"), "version 1\n").unwrap();
    fs::write(root.join("README.md"), "readme\n").unwrap();
    run_git(root, &["add", "."]);
    // `git commit` writes the `TREE` cache-tree extension into `.git/index`
    run_git(root, &["commit", "-m", "initial"]);

    // Modify `a/b/c/deep.txt` and stage it via `gir`'s `GitIndex`
    let deep_abs = root.join("a/b/c/deep.txt");
    fs::write(&deep_abs, "version 2 staged by gir\n").unwrap();

    let repo = Repository::discover(root).unwrap();
    let mut index = GitIndex::read(repo.git_dir()).unwrap();
    let meta = fs::symlink_metadata(&deep_abs).unwrap();
    let new_oid = repo
        .odb()
        .write_raw(ObjectKind::Blob, b"version 2 staged by gir\n")
        .unwrap();
    index.add_entry(IndexEntry::from_metadata(
        "a/b/c/deep.txt",
        new_oid,
        0,
        &meta,
    ));
    index.write(repo.git_dir()).unwrap();

    // Commit using official `/usr/bin/git commit` — if `TREE` wasn't invalidated, `git commit` would ignore the change!
    run_git(root, &["commit", "-m", "second commit"]);

    let cat_out = run_git(root, &["cat-file", "-p", "HEAD:a/b/c/deep.txt"]);
    assert_eq!(cat_out.stdout, b"version 2 staged by gir\n");
}

#[test]
fn gitignore_and_porcelain_status_differential() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    run_git(root, &["init", "-b", "main"]);
    fs::write(
        root.join(".gitignore"),
        "# comment line\n*.log\n!important.log\nbuild/\n/root_only.tmp\n**/cache/**\n",
    )
    .unwrap();

    fs::create_dir_all(root.join("src/sub")).unwrap();
    fs::write(root.join("src/.gitignore"), "*.bak\n").unwrap();
    fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
    fs::write(root.join("src/keep.rs"), "pub fn keep() {}\n").unwrap();
    fs::write(root.join("src/delete_me.rs"), "pub fn del() {}\n").unwrap();
    run_git(root, &["add", "."]);
    run_git(root, &["commit", "-m", "base"]);

    // 1. Create ignored files that must be filtered out
    fs::write(root.join("app.log"), "ignored\n").unwrap();
    fs::write(root.join("src/sub/nested.log"), "ignored\n").unwrap();
    fs::write(root.join("src/sub/file.bak"), "ignored by nested gitignore\n").unwrap();
    fs::write(root.join("root_only.tmp"), "ignored at root\n").unwrap();
    fs::create_dir_all(root.join("build/out")).unwrap();
    fs::write(root.join("build/out/bin"), "ignored dir\n").unwrap();
    fs::create_dir_all(root.join("src/cache/deep")).unwrap();
    fs::write(root.join("src/cache/deep/item"), "ignored doublestar\n").unwrap();

    // 2. Create negated & unanchored matches that MUST appear as untracked
    fs::write(root.join("important.log"), "negated!\n").unwrap();
    fs::write(root.join("src/sub/important.log"), "negated in subdir!\n").unwrap();
    fs::write(root.join("src/root_only.tmp"), "not at root!\n").unwrap();

    // 3. Create a wholly untracked directory (should collapse to `untracked_dir/`)
    fs::create_dir_all(root.join("untracked_dir/inner")).unwrap();
    fs::write(root.join("untracked_dir/inner/a.txt"), "untracked\n").unwrap();

    // 4. Create staged + unstaged modifications
    fs::write(root.join("src/main.rs"), "fn main() { /* staged */ }\n").unwrap();
    run_git(root, &["add", "src/main.rs"]);
    fs::write(
        root.join("src/main.rs"),
        "fn main() { /* staged + unstaged */ }\n",
    )
    .unwrap();
    fs::remove_file(root.join("src/delete_me.rs")).unwrap();

    let repo = Repository::discover(root).unwrap();
    let report = compute_status(&repo, StatusOptions::default()).unwrap();
    let gir_porcelain = report.format_porcelain(false);

    let git_out = run_git(root, &["status", "--porcelain=v1"]);
    let git_porcelain = String::from_utf8(git_out.stdout).unwrap();

    assert_eq!(gir_porcelain, git_porcelain);
}
