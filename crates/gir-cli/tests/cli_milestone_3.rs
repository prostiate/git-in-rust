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
fn cli_symbolic_ref_update_ref_rev_parse_reflog_and_undo() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    run_git(root, &["init", "-b", "main"]);
    fs::write(root.join("hello.txt"), "v1\n").unwrap();
    run_git(root, &["add", "."]);
    run_git(root, &["commit", "-m", "first"]);

    fs::write(root.join("hello.txt"), "v2\n").unwrap();
    run_git(root, &["add", "."]);
    run_git(root, &["commit", "-m", "second"]);

    // 1. `gir symbolic-ref HEAD` and `--short`
    let sym_out = Command::new(gir_bin())
        .current_dir(root)
        .args(["symbolic-ref", "--short", "HEAD"])
        .output()
        .unwrap();
    assert!(sym_out.status.success());
    assert_eq!(String::from_utf8(sym_out.stdout).unwrap().trim(), "main");

    // 2. `gir rev-parse HEAD~1` vs `git rev-parse HEAD~1`
    let gir_rp = Command::new(gir_bin())
        .current_dir(root)
        .args(["rev-parse", "HEAD~1"])
        .output()
        .unwrap();
    let git_rp = run_git(root, &["rev-parse", "HEAD~1"]);
    assert_eq!(gir_rp.stdout, git_rp.stdout);
    let c1_hex = String::from_utf8(git_rp.stdout).unwrap().trim().to_string();

    // 3. `gir update-ref refs/heads/topic <c1>`
    let up_out = Command::new(gir_bin())
        .current_dir(root)
        .args(["update-ref", "-m", "branch: Created topic", "refs/heads/topic", &c1_hex])
        .output()
        .unwrap();
    assert!(up_out.status.success());

    // 4. `gir reflog show topic`
    let rl_out = Command::new(gir_bin())
        .current_dir(root)
        .args(["reflog", "show", "topic"])
        .output()
        .unwrap();
    assert!(rl_out.status.success());
    let rl_str = String::from_utf8(rl_out.stdout).unwrap();
    assert!(rl_str.contains("topic@{0}: branch: Created topic"));

    // 5. `gir undo --snapshot "before experiment"` -> modify file -> `gir undo`
    fs::write(root.join("hello.txt"), "uncommitted precious work\n").unwrap();
    let snap_out = Command::new(gir_bin())
        .current_dir(root)
        .args(["undo", "--snapshot", "before wipe"])
        .output()
        .unwrap();
    assert!(snap_out.status.success());

    fs::write(root.join("hello.txt"), "destroyed!\n").unwrap();
    let undo_out = Command::new(gir_bin())
        .current_dir(root)
        .args(["undo"])
        .output()
        .unwrap();
    assert!(undo_out.status.success());
    assert_eq!(
        fs::read_to_string(root.join("hello.txt")).unwrap(),
        "uncommitted precious work\n"
    );
}
