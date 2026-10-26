use std::fs;
use std::process::Command;

use tempfile::TempDir;

fn gir_bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_gir"))
}

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
        .unwrap();
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

#[test]
fn cli_add_status_ls_files_mv_and_rm_end_to_end() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    run_git(root, &["init", "-b", "main"]);
    fs::write(root.join(".gitignore"), "*.tmp\n").unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/a.rs"), "pub fn a() {}\n").unwrap();
    fs::write(root.join("src/b.rs"), "pub fn b() {}\n").unwrap();
    fs::write(root.join("ignored.tmp"), "ignore me\n").unwrap();

    // 1. `gir add .`
    let add_out = Command::new(gir_bin())
        .current_dir(root)
        .args(["add", "."])
        .output()
        .unwrap();
    assert!(add_out.status.success());

    // 2. Compare `gir ls-files --stage` with `git ls-files --stage`
    let gir_ls = Command::new(gir_bin())
        .current_dir(root)
        .args(["ls-files", "--stage"])
        .output()
        .unwrap();
    let git_ls = run_git(root, &["ls-files", "--stage"]);
    assert_eq!(gir_ls.stdout, git_ls.stdout);

    // Commit via official git so we have a clean HEAD
    run_git(root, &["commit", "-m", "initial"]);

    // 3. `gir mv src/a.rs src/renamed.rs`
    let mv_out = Command::new(gir_bin())
        .current_dir(root)
        .args(["mv", "src/a.rs", "src/renamed.rs"])
        .output()
        .unwrap();
    assert!(mv_out.status.success());
    assert!(!root.join("src/a.rs").exists());
    assert!(root.join("src/renamed.rs").is_file());

    // 4. `gir rm src/b.rs`
    let rm_out = Command::new(gir_bin())
        .current_dir(root)
        .args(["rm", "src/b.rs"])
        .output()
        .unwrap();
    assert!(rm_out.status.success());
    assert!(!root.join("src/b.rs").exists());

    // 5. Verify `gir status -s`
    let st_out = Command::new(gir_bin())
        .current_dir(root)
        .args(["status", "-s"])
        .output()
        .unwrap();
    assert!(st_out.status.success());
    let st_str = String::from_utf8(st_out.stdout).unwrap();
    assert!(st_str.contains("D  src/a.rs"));
    assert!(st_str.contains("D  src/b.rs"));
    assert!(st_str.contains("A  src/renamed.rs"));
}
