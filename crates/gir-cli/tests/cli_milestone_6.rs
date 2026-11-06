use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};

use tempfile::TempDir;

fn gir_bin() -> String {
    env!("CARGO_BIN_EXE_gir").to_string()
}

#[test]
fn cli_stash_interoperability_between_gir_and_official_git() {
    let dir = TempDir::new().unwrap();

    assert!(Command::new(gir_bin())
        .args(["init"])
        .current_dir(dir.path())
        .status()
        .unwrap()
        .success());
    assert!(Command::new(gir_bin())
        .args(["config", "user.name", "Irfan"])
        .current_dir(dir.path())
        .status()
        .unwrap()
        .success());
    assert!(Command::new(gir_bin())
        .args(["config", "user.email", "irfan@example.com"])
        .current_dir(dir.path())
        .status()
        .unwrap()
        .success());

    fs::write(dir.path().join("tracked.txt"), "base line\n").unwrap();
    assert!(Command::new(gir_bin())
        .args(["add", "."])
        .current_dir(dir.path())
        .status()
        .unwrap()
        .success());
    assert!(Command::new(gir_bin())
        .args(["commit", "-m", "initial"])
        .current_dir(dir.path())
        .status()
        .unwrap()
        .success());

    // 1. Modify `tracked.txt` + create `untracked.txt`, stash with `gir stash push -u -m "from gir"`,
    //    and pop using official `/usr/bin/git stash pop`!
    fs::write(dir.path().join("tracked.txt"), "modified by gir\n").unwrap();
    fs::write(dir.path().join("untracked.txt"), "untracked by gir\n").unwrap();

    assert!(Command::new(gir_bin())
        .args(["stash", "push", "-u", "-m", "from gir"])
        .current_dir(dir.path())
        .status()
        .unwrap()
        .success());

    assert_eq!(
        fs::read_to_string(dir.path().join("tracked.txt")).unwrap(),
        "base line\n"
    );
    assert!(!dir.path().join("untracked.txt").exists());

    // Verify `/usr/bin/git stash list` sees the stash created by `gir`
    let git_list = Command::new("git")
        .args(["stash", "list"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(git_list.status.success());
    assert!(String::from_utf8_lossy(&git_list.stdout).contains("from gir"));

    // Pop with `/usr/bin/git stash pop`!
    let git_pop = Command::new("git")
        .args(["stash", "pop", "-q"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        git_pop.status.success(),
        "git stash pop failed: {}",
        String::from_utf8_lossy(&git_pop.stderr)
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("tracked.txt")).unwrap(),
        "modified by gir\n"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("untracked.txt")).unwrap(),
        "untracked by gir\n"
    );

    // 2. Reverse direction: stash with `/usr/bin/git stash push -u -m "from official git"`,
    //    and pop with `gir stash pop`!
    assert!(Command::new("git")
        .args(["stash", "push", "-q", "-u", "-m", "from official git"])
        .current_dir(dir.path())
        .status()
        .unwrap()
        .success());
    assert!(!dir.path().join("untracked.txt").exists());

    let gir_list = Command::new(gir_bin())
        .args(["stash", "list"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&gir_list.stdout).contains("from official git"));

    assert!(Command::new(gir_bin())
        .args(["stash", "pop"])
        .current_dir(dir.path())
        .status()
        .unwrap()
        .success());
    assert_eq!(
        fs::read_to_string(dir.path().join("tracked.txt")).unwrap(),
        "modified by gir\n"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("untracked.txt")).unwrap(),
        "untracked by gir\n"
    );
}

#[test]
fn cli_interactive_add_patch_stages_selected_hunk_only() {
    let dir = TempDir::new().unwrap();

    assert!(Command::new(gir_bin())
        .args(["init"])
        .current_dir(dir.path())
        .status()
        .unwrap()
        .success());
    assert!(Command::new(gir_bin())
        .args(["config", "user.name", "Irfan"])
        .current_dir(dir.path())
        .status()
        .unwrap()
        .success());
    assert!(Command::new(gir_bin())
        .args(["config", "user.email", "irfan@example.com"])
        .current_dir(dir.path())
        .status()
        .unwrap()
        .success());

    let base: String = (1..=20).map(|i| format!("line {i}\n")).collect();
    fs::write(dir.path().join("app.rs"), &base).unwrap();
    assert!(Command::new(gir_bin())
        .args(["add", "app.rs"])
        .current_dir(dir.path())
        .status()
        .unwrap()
        .success());
    assert!(Command::new(gir_bin())
        .args(["commit", "-m", "init"])
        .current_dir(dir.path())
        .status()
        .unwrap()
        .success());

    // Edit Hunk 1 (line 2) and Hunk 2 (line 19)
    let mut lines: Vec<String> = (1..=20).map(|i| format!("line {i}")).collect();
    lines[1] = "line 2 STAGED HUNK".to_string();
    lines[18] = "line 19 UNSTAGED HUNK".to_string();
    fs::write(dir.path().join("app.rs"), format!("{}\n", lines.join("\n"))).unwrap();

    // Run `gir add -p` answering `y` to hunk 1 and `n` to hunk 2
    let mut child = Command::new(gir_bin())
        .args(["add", "-p", "app.rs"])
        .current_dir(dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();

    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"y\nn\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());

    // Verify `gir diff --staged` shows ONLY Hunk 1 (`line 2 STAGED HUNK`)
    let staged_diff = Command::new(gir_bin())
        .args(["diff", "--staged"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let staged_str = String::from_utf8_lossy(&staged_diff.stdout);
    assert!(staged_str.contains("+line 2 STAGED HUNK"));
    assert!(!staged_str.contains("line 19 UNSTAGED HUNK"));

    // Verify `gir diff` (worktree vs index) shows ONLY Hunk 2 (`line 19 UNSTAGED HUNK`)
    let wt_diff = Command::new(gir_bin())
        .args(["diff"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let wt_str = String::from_utf8_lossy(&wt_diff.stdout);
    assert!(!wt_str.contains("line 2 STAGED HUNK"));
    assert!(wt_str.contains("+line 19 UNSTAGED HUNK"));
}
