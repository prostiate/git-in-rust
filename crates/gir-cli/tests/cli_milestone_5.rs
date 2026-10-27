use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use tempfile::TempDir;

fn gir_bin() -> String {
    env!("CARGO_BIN_EXE_gir").to_string()
}

#[test]
fn cli_commit_exact_sha_match_and_hooks() {
    let dir_gir = TempDir::new().unwrap();
    let dir_git = TempDir::new().unwrap();

    for d in [dir_gir.path(), dir_git.path()] {
        fs::create_dir_all(d.join("src")).unwrap();
        fs::write(d.join("src/main.rs"), "fn main() {\n    println!(\"hi\");\n}\n").unwrap();
        fs::write(d.join("README.md"), "# Demo\n").unwrap();
    }

    // Initialize and commit with `gir`
    assert!(Command::new(gir_bin())
        .args(["init"])
        .current_dir(dir_gir.path())
        .status()
        .unwrap()
        .success());
    assert!(Command::new(gir_bin())
        .args(["branch", "-M", "main"])
        .current_dir(dir_gir.path())
        .status()
        .unwrap()
        .success());
    assert!(Command::new(gir_bin())
        .args(["add", "."])
        .current_dir(dir_gir.path())
        .status()
        .unwrap()
        .success());

    let envs = [
        ("GIT_AUTHOR_NAME", "Irfan"),
        ("GIT_AUTHOR_EMAIL", "irfan@example.com"),
        ("GIT_AUTHOR_DATE", "1730000000 +0700"),
        ("GIT_COMMITTER_NAME", "Irfan"),
        ("GIT_COMMITTER_EMAIL", "irfan@example.com"),
        ("GIT_COMMITTER_DATE", "1730000000 +0700"),
    ];

    let gir_commit = Command::new(gir_bin())
        .args(["commit", "-m", "feat: initial commit"])
        .envs(envs)
        .current_dir(dir_gir.path())
        .output()
        .unwrap();
    assert!(
        gir_commit.status.success(),
        "gir commit failed: {}",
        String::from_utf8_lossy(&gir_commit.stderr)
    );

    // Initialize and commit with `/usr/bin/git`
    assert!(Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(dir_git.path())
        .status()
        .unwrap()
        .success());
    assert!(Command::new("git")
        .args(["add", "."])
        .current_dir(dir_git.path())
        .status()
        .unwrap()
        .success());
    assert!(Command::new("git")
        .args(["commit", "-q", "-m", "feat: initial commit"])
        .envs(envs)
        .current_dir(dir_git.path())
        .status()
        .unwrap()
        .success());

    // Compare 40-char commit SHAs: must be 100% identical!
    let gir_head = Command::new(gir_bin())
        .args(["rev-parse", "HEAD"])
        .current_dir(dir_gir.path())
        .output()
        .unwrap();
    let git_head = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir_git.path())
        .output()
        .unwrap();
    assert_eq!(gir_head.stdout, git_head.stdout);

    // Test `commit-msg` hook mutating the commit message
    let hook_path = dir_gir.path().join(".git/hooks/commit-msg");
    fs::write(
        &hook_path,
        "#!/bin/sh\necho \"Signed-off-by: Hook <hook@example.com>\" >> \"$1\"\n",
    )
    .unwrap();
    fs::set_permissions(&hook_path, fs::Permissions::from_mode(0o755)).unwrap();

    fs::write(dir_gir.path().join("README.md"), "# Demo v2\n").unwrap();
    assert!(Command::new(gir_bin())
        .args(["commit", "-a", "-m", "docs: update readme"])
        .envs(envs)
        .current_dir(dir_gir.path())
        .status()
        .unwrap()
        .success());

    let show_out = Command::new(gir_bin())
        .args(["show", "--stat"])
        .current_dir(dir_gir.path())
        .output()
        .unwrap();
    assert!(show_out.status.success());
    let show_str = String::from_utf8_lossy(&show_out.stdout);
    assert!(show_str.contains("docs: update readme"));
    assert!(show_str.contains("Signed-off-by: Hook <hook@example.com>"));
    assert!(show_str.contains("README.md"));
}

#[test]
fn cli_log_range_and_graph_match_official_git() {
    let dir = TempDir::new().unwrap();

    // Build a multi-branch merge DAG using official `git` and verify `gir log` matches 1:1
    assert!(Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(dir.path())
        .status()
        .unwrap()
        .success());

    let run_git_commit = |msg: &str, file: &str, content: &str, ts: i64| {
        fs::write(dir.path().join(file), content).unwrap();
        Command::new("git")
            .args(["add", file])
            .current_dir(dir.path())
            .status()
            .unwrap();
        let date = format!("{ts} +0000");
        Command::new("git")
            .args(["commit", "-q", "-m", msg])
            .envs([
                ("GIT_AUTHOR_NAME", "Irfan"),
                ("GIT_AUTHOR_EMAIL", "irfan@example.com"),
                ("GIT_AUTHOR_DATE", date.as_str()),
                ("GIT_COMMITTER_NAME", "Irfan"),
                ("GIT_COMMITTER_EMAIL", "irfan@example.com"),
                ("GIT_COMMITTER_DATE", date.as_str()),
            ])
            .current_dir(dir.path())
            .status()
            .unwrap();
    };

    run_git_commit("c1: base", "base.txt", "base\n", 1730000001);
    assert!(Command::new(gir_bin())
        .args(["branch", "feature"])
        .current_dir(dir.path())
        .status()
        .unwrap()
        .success());

    run_git_commit("c2: main work", "main.txt", "main\n", 1730000002);

    assert!(Command::new("git")
        .args(["checkout", "-q", "feature"])
        .current_dir(dir.path())
        .status()
        .unwrap()
        .success());
    run_git_commit("c3: feat 1", "feat1.txt", "f1\n", 1730000003);
    run_git_commit("c4: feat 2", "feat2.txt", "f2\n", 1730000004);

    // Compare `gir log --oneline --no-decorate main..feature` with `/usr/bin/git log --oneline --no-decorate main..feature`
    let gir_log = Command::new(gir_bin())
        .args(["log", "--oneline", "--no-decorate", "main..feature"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let git_log = Command::new("git")
        .args(["log", "--oneline", "--no-decorate", "main..feature"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(gir_log.status.success());
    assert_eq!(
        String::from_utf8_lossy(&gir_log.stdout),
        String::from_utf8_lossy(&git_log.stdout)
    );

    // Compare symmetric range `main...feature`
    let gir_sym = Command::new(gir_bin())
        .args(["log", "--oneline", "--no-decorate", "main...feature"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let git_sym = Command::new("git")
        .args(["log", "--oneline", "--no-decorate", "main...feature"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&gir_sym.stdout),
        String::from_utf8_lossy(&git_sym.stdout)
    );

    // Verify `gir tag -a -m ...` and `gir show`
    assert!(Command::new(gir_bin())
        .args(["tag", "-a", "-m", "Tagging feature tip", "v1.0.0"])
        .envs([
            ("GIT_COMMITTER_NAME", "Irfan"),
            ("GIT_COMMITTER_EMAIL", "irfan@example.com"),
            ("GIT_COMMITTER_DATE", "1730000010 +0000"),
        ])
        .current_dir(dir.path())
        .status()
        .unwrap()
        .success());

    let gir_show_tag = Command::new(gir_bin())
        .args(["show", "--no-patch", "v1.0.0"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(gir_show_tag.status.success());
    let tag_out = String::from_utf8_lossy(&gir_show_tag.stdout);
    assert!(tag_out.contains("tag v1.0.0"));
    assert!(tag_out.contains("Tagging feature tip"));
    assert!(tag_out.contains("c4: feat 2"));

    // Verify `gir log --graph --oneline --all`
    let gir_graph = Command::new(gir_bin())
        .args(["log", "--graph", "--oneline", "--all"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(gir_graph.status.success());
    let graph_out = String::from_utf8_lossy(&gir_graph.stdout);
    assert!(graph_out.contains("* "));
    assert!(graph_out.contains("c4: feat 2"));
    assert!(graph_out.contains("c2: main work"));
    assert!(graph_out.contains("c1: base"));
}
