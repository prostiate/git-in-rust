use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

fn gir_bin() -> String {
    env!("CARGO_BIN_EXE_gir").to_string()
}

fn run_gir(dir: &Path, args: &[&str]) -> String {
    let out = Command::new(gir_bin())
        .current_dir(dir)
        .args(args)
        .output()
        .expect("run gir");
    assert!(
        out.status.success(),
        "gir {:?} failed:\nstdout: {}\nstderr: {}",
        args,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn cli_remote_clone_push_fetch_and_pull_workflow() {
    let tmp = TempDir::new().unwrap();
    let bare_remote = tmp.path().join("origin.git");
    let dev_a = tmp.path().join("dev-a");
    let dev_b = tmp.path().join("dev-b");

    // 1. Initialize bare remote repository (`gir init --bare`)
    fs::create_dir_all(&bare_remote).unwrap();
    run_gir(&bare_remote, &["init", "--bare"]);

    // 2. Initialize `dev-a`, test `gir remote add / -v / set-url / rename`, and push initial commit
    fs::create_dir_all(&dev_a).unwrap();
    run_gir(&dev_a, &["init"]);
    run_gir(&dev_a, &["config", "user.name", "Alice"]);
    run_gir(&dev_a, &["config", "user.email", "alice@example.com"]);

    run_gir(&dev_a, &["remote", "add", "upstream", "https://example.com/old.git"]);
    assert!(run_gir(&dev_a, &["remote", "-v"]).contains("upstream\thttps://example.com/old.git (fetch)"));
    run_gir(
        &dev_a,
        &["remote", "set-url", "upstream", bare_remote.to_str().unwrap()],
    );
    run_gir(&dev_a, &["remote", "rename", "upstream", "origin"]);
    assert_eq!(run_gir(&dev_a, &["remote"]), "origin");

    fs::write(dev_a.join("app.txt"), "line 1\n").unwrap();
    run_gir(&dev_a, &["add", "app.txt"]);
    run_gir(&dev_a, &["commit", "-m", "Initial commit from Alice"]);

    // `gir push -u origin main`
    run_gir(&dev_a, &["push", "-u", "origin", "main"]);

    // 3. Clone `origin.git` into `dev-b` via `gir clone`
    run_gir(
        tmp.path(),
        &["clone", bare_remote.to_str().unwrap(), dev_b.to_str().unwrap()],
    );
    run_gir(&dev_b, &["config", "user.name", "Bob"]);
    run_gir(&dev_b, &["config", "user.email", "bob@example.com"]);
    assert_eq!(fs::read_to_string(dev_b.join("app.txt")).unwrap(), "line 1\n");

    // 4. Bob commits and pushes to `origin`
    fs::write(dev_b.join("app.txt"), "line 1\nline 2 from Bob\n").unwrap();
    run_gir(&dev_b, &["add", "app.txt"]);
    run_gir(&dev_b, &["commit", "-m", "Bob adds line 2"]);
    run_gir(&dev_b, &["push"]);

    // 5. Alice runs `gir pull` (fast-forward) and receives Bob's changes
    let pull_out = run_gir(&dev_a, &["pull"]);
    assert!(pull_out.contains("Fast-forward"));
    assert_eq!(
        fs::read_to_string(dev_a.join("app.txt")).unwrap(),
        "line 1\nline 2 from Bob\n"
    );

    // 6. Test `gir pull --rebase`: Bob pushes a commit while Alice has a local commit on another file
    fs::write(dev_b.join("bob.txt"), "bob feature\n").unwrap();
    run_gir(&dev_b, &["add", "bob.txt"]);
    run_gir(&dev_b, &["commit", "-m", "Bob adds bob.txt"]);
    run_gir(&dev_b, &["push"]);

    fs::write(dev_a.join("alice.txt"), "alice feature\n").unwrap();
    run_gir(&dev_a, &["add", "alice.txt"]);
    run_gir(&dev_a, &["commit", "-m", "Alice adds alice.txt"]);

    run_gir(&dev_a, &["pull", "--rebase"]);
    assert_eq!(fs::read_to_string(dev_a.join("bob.txt")).unwrap(), "bob feature\n");
    assert_eq!(
        fs::read_to_string(dev_a.join("alice.txt")).unwrap(),
        "alice feature\n"
    );
    run_gir(&dev_a, &["fsck"]);

    // 7. Test pushing a feature branch and deleting it remotely via `gir push origin --delete temp-branch`
    run_gir(&dev_a, &["branch", "temp-branch"]);
    run_gir(&dev_a, &["push", "origin", "temp-branch"]);
    assert!(bare_remote.join("refs/heads/temp-branch").is_file());
    run_gir(&dev_a, &["push", "origin", "--delete", "temp-branch"]);
    assert!(!bare_remote.join("refs/heads/temp-branch").exists());
    assert!(!dev_a.join(".git/refs/remotes/origin/temp-branch").exists());

    // 8. Test `gir remote rename` migrates `branch.main.remote` and `refs/remotes/origin/*`
    run_gir(&dev_a, &["remote", "rename", "origin", "primary"]);
    assert_eq!(run_gir(&dev_a, &["config", "branch.main.remote"]), "primary");
    assert!(dev_a.join(".git/refs/remotes/primary/main").is_file());
}
