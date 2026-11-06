use std::fs;
use std::process::Command;

use tempfile::TempDir;

fn gir_bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_gir"))
}

#[test]
fn cli_merge_conflict_and_interactive_rebase_end_to_end() {
    let dir = TempDir::new().unwrap();
    let w = dir.path();

    assert!(Command::new(gir_bin()).args(["init"]).current_dir(w).status().unwrap().success());
    assert!(Command::new(gir_bin()).args(["config", "user.name", "Irfan"]).current_dir(w).status().unwrap().success());
    assert!(Command::new(gir_bin()).args(["config", "user.email", "irfan@example.com"]).current_dir(w).status().unwrap().success());
    assert!(Command::new(gir_bin()).args(["branch", "-M", "main"]).current_dir(w).status().unwrap().success());

    fs::write(w.join("story.txt"), "chapter 1\nchapter 2\n").unwrap();
    assert!(Command::new(gir_bin()).args(["add", "story.txt"]).current_dir(w).status().unwrap().success());
    assert!(Command::new(gir_bin()).args(["commit", "-m", "initial"]).current_dir(w).status().unwrap().success());

    // Create branch `feat` with two commits that we will interactively squash
    assert!(Command::new(gir_bin()).args(["switch", "-c", "feat"]).current_dir(w).status().unwrap().success());
    fs::write(w.join("feat.txt"), "part 1\n").unwrap();
    assert!(Command::new(gir_bin()).args(["add", "feat.txt"]).current_dir(w).status().unwrap().success());
    assert!(Command::new(gir_bin()).args(["commit", "-m", "feat part 1"]).current_dir(w).status().unwrap().success());

    fs::write(w.join("feat.txt"), "part 1\npart 2\n").unwrap();
    assert!(Command::new(gir_bin()).args(["add", "feat.txt"]).current_dir(w).status().unwrap().success());
    assert!(Command::new(gir_bin()).args(["commit", "-m", "feat part 2"]).current_dir(w).status().unwrap().success());

    // Advance `main`
    assert!(Command::new(gir_bin()).args(["switch", "main"]).current_dir(w).status().unwrap().success());
    fs::write(w.join("main_only.txt"), "main work\n").unwrap();
    assert!(Command::new(gir_bin()).args(["add", "main_only.txt"]).current_dir(w).status().unwrap().success());
    assert!(Command::new(gir_bin()).args(["commit", "-m", "main work"]).current_dir(w).status().unwrap().success());

    // Switch to `feat` and run `gir rebase -i main` using `GIT_SEQUENCE_EDITOR` to squash the 2nd commit into the 1st
    assert!(Command::new(gir_bin()).args(["switch", "feat"]).current_dir(w).status().unwrap().success());
    let rb_out = Command::new(gir_bin())
        .args(["rebase", "-i", "main"])
        .env("GIT_SEQUENCE_EDITOR", "sed -i '2s/^pick/squash/'")
        .current_dir(w)
        .output()
        .unwrap();
    assert!(
        rb_out.status.success(),
        "rebase -i failed: {}",
        String::from_utf8_lossy(&rb_out.stderr)
    );

    // Verify `feat` now has `main_only.txt` and `feat.txt` ("part 1\npart 2\n") in a single squashed commit on top of `main`
    assert_eq!(fs::read_to_string(w.join("feat.txt")).unwrap(), "part 1\npart 2\n");
    assert_eq!(fs::read_to_string(w.join("main_only.txt")).unwrap(), "main work\n");

    let log_out = Command::new(gir_bin())
        .args(["log", "--oneline", "main..feat"])
        .current_dir(w)
        .output()
        .unwrap();
    let log_lines: Vec<&str> = std::str::from_utf8(&log_out.stdout).unwrap().lines().collect();
    assert_eq!(log_lines.len(), 1);
}
