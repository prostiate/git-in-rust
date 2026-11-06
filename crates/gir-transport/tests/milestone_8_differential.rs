use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::sync::Arc;
use std::thread;

use gir_core::commit_ops::{create_commit, CommitOptions};
use gir_core::index::{GitIndex, IndexEntry};
use gir_core::{ObjectKind, Repository};
use gir_transport::server::{
    advertise_receive_pack_refs, advertise_upload_pack_refs, handle_receive_pack,
    handle_upload_pack,
};
use gir_transport::{clone_repository, fetch_from_remote, push_to_remote, PushOptions};
use tempfile::TempDir;

fn run_git(dir: &std::path::Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Test User")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test User")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn configure_identity(repo: &Repository) {
    let mut cfg = repo.local_config().unwrap();
    cfg.set("user.name", "Test User").unwrap();
    cfg.set("user.email", "test@example.com").unwrap();
    cfg.save_to(repo.git_dir().join("config")).unwrap();
}

fn stage_and_commit(repo: &Repository, path: &str, content: &str, msg: &str) {
    let wt = repo.worktree().unwrap();
    let full = wt.join(path);
    if let Some(p) = full.parent() {
        fs::create_dir_all(p).unwrap();
    }
    fs::write(&full, content).unwrap();
    let meta = fs::symlink_metadata(&full).unwrap();
    let oid = repo
        .odb()
        .write_raw(ObjectKind::Blob, content.as_bytes())
        .unwrap();
    let mut index = GitIndex::read(repo.git_dir()).unwrap();
    index.add_entry(IndexEntry::from_metadata(path, oid, 0, &meta));
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
fn differential_clone_push_fetch_and_pre_push_hook_interop_with_official_git() {
    let tmp = TempDir::new().unwrap();
    let remote_bare = tmp.path().join("remote.git");
    let seed_dir = tmp.path().join("seed");
    let gir_clone_dir = tmp.path().join("gir-clone");
    let git_clone_dir = tmp.path().join("git-clone");

    // 1. Initialize bare repository with official `git init --bare`
    fs::create_dir_all(&remote_bare).unwrap();
    run_git(&remote_bare, &["init", "--bare", "--initial-branch=main"]);

    // 2. Create seed repository with official `git`, commit initial files, and push to `remote.git`
    fs::create_dir_all(&seed_dir).unwrap();
    run_git(&seed_dir, &["init", "--initial-branch=main"]);
    fs::write(seed_dir.join("README.md"), "# Gir Project\n").unwrap();
    fs::create_dir_all(seed_dir.join("src")).unwrap();
    fs::write(seed_dir.join("src/lib.rs"), "pub fn v1() -> u32 { 1 }\n").unwrap();
    run_git(&seed_dir, &["add", "."]);
    run_git(&seed_dir, &["commit", "-m", "Initial seed commit"]);
    run_git(
        &seed_dir,
        &["remote", "add", "origin", remote_bare.to_str().unwrap()],
    );
    run_git(&seed_dir, &["push", "-u", "origin", "main"]);

    // 3. Clone `remote.git` using `gir_transport::clone_repository`
    let gir_repo = clone_repository(remote_bare.to_str().unwrap(), &gir_clone_dir, false).unwrap();
    configure_identity(&gir_repo);

    assert_eq!(
        fs::read_to_string(gir_clone_dir.join("README.md")).unwrap(),
        "# Gir Project\n"
    );
    assert_eq!(
        fs::read_to_string(gir_clone_dir.join("src/lib.rs")).unwrap(),
        "pub fn v1() -> u32 { 1 }\n"
    );

    // Verify official `git status` and `git fsck --full` inside the `gir`-cloned repository
    assert_eq!(run_git(&gir_clone_dir, &["status", "--porcelain"]), "");
    run_git(&gir_clone_dir, &["fsck", "--full"]);

    // 4. Install a `pre-push` hook that logs its stdin and can reject pushes when `REJECT_PUSH=1`
    let hooks_dir = gir_repo.git_dir().join("hooks");
    fs::create_dir_all(&hooks_dir).unwrap();
    let hook_log = gir_repo.git_dir().join("pre-push.log");
    let hook_path = hooks_dir.join("pre-push");
    fs::write(
        &hook_path,
        format!(
            "#!/bin/sh\ncat > \"{}\"\nif [ -f \"{}\" ]; then exit 1; fi\nexit 0\n",
            hook_log.display(),
            gir_repo.git_dir().join("block_push").display()
        ),
    )
    .unwrap();
    let mut perms = fs::metadata(&hook_path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&hook_path, perms).unwrap();

    // Create a new commit in `gir_clone_dir` and test `pre-push` rejection first
    stage_and_commit(
        &gir_repo,
        "src/lib.rs",
        "pub fn v1() -> u32 { 2 }\n",
        "Update v1 to return 2",
    );
    fs::write(gir_repo.git_dir().join("block_push"), "1").unwrap();
    let blocked = push_to_remote(&gir_repo, &PushOptions::default());
    assert!(blocked.is_err());

    // Unblock `pre-push` and push with `set_upstream: true`
    fs::remove_file(gir_repo.git_dir().join("block_push")).unwrap();
    let push_out = push_to_remote(
        &gir_repo,
        &PushOptions {
            remote: Some("origin".to_string()),
            refspecs: vec!["main".to_string()],
            set_upstream: true,
            force: false,
            no_verify: false,
        },
    )
    .unwrap();
    assert!(!push_out.up_to_date);
    assert_eq!(push_out.updates.len(), 1);
    assert!(fs::read_to_string(&hook_log)
        .unwrap()
        .contains("refs/heads/main"));

    // 5. Verify `remote.git` passes `git fsck --full` and official `git clone` sees our pushed commit!
    run_git(&remote_bare, &["fsck", "--full"]);
    run_git(
        tmp.path(),
        &[
            "clone",
            remote_bare.to_str().unwrap(),
            git_clone_dir.to_str().unwrap(),
        ],
    );
    assert_eq!(
        fs::read_to_string(git_clone_dir.join("src/lib.rs")).unwrap(),
        "pub fn v1() -> u32 { 2 }\n"
    );

    // 6. Commit in `git_clone_dir` and push with official `git push`, then `fetch_from_remote` in `gir_repo`
    fs::write(git_clone_dir.join("from_git.txt"), "pushed by official git\n").unwrap();
    run_git(&git_clone_dir, &["add", "from_git.txt"]);
    run_git(&git_clone_dir, &["commit", "-m", "Commit from official git"]);
    let expected_oid = run_git(&git_clone_dir, &["rev-parse", "HEAD"]);
    run_git(&git_clone_dir, &["push", "origin", "main"]);

    let fetch_out = fetch_from_remote(&gir_repo, "origin", &[]).unwrap();
    assert!(fetch_out.objects_received);
    let tracked_oid = gir_repo
        .refs()
        .resolve_to_oid("refs/remotes/origin/main")
        .unwrap()
        .to_hex();
    assert_eq!(tracked_oid, expected_oid);
}

#[test]
fn smart_http_wire_protocol_end_to_end_with_gir_and_official_git() {
    let tmp = TempDir::new().unwrap();
    let server_bare_dir = tmp.path().join("server.git");
    let gir_client_dir = tmp.path().join("http-client");
    let git_client_dir = tmp.path().join("git-http-client");

    // 1. Initialize bare repository managed by `gir-transport::server`
    let server_repo = Arc::new(Repository::init(&server_bare_dir, true).unwrap());

    // Spawn a minimal Smart HTTP/1.1 server on an ephemeral localhost port
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server_repo_bg = Arc::clone(&server_repo);

    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let repo = Arc::clone(&server_repo_bg);
            handle_smart_http_connection(stream, &repo);
        }
    });

    let base_url = format!("http://127.0.0.1:{port}/server.git");

    // 2. Initialize a local `gir` client repository, commit files, and `gir push` over Smart HTTP!
    let client_repo = Repository::init(&gir_client_dir, false).unwrap();
    configure_identity(&client_repo);
    {
        let mut cfg = client_repo.local_config().unwrap();
        cfg.set("remote.origin.url", &base_url).unwrap();
        cfg.set(
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        )
        .unwrap();
        cfg.save_to(client_repo.git_dir().join("config")).unwrap();
    }

    stage_and_commit(
        &client_repo,
        "hello.txt",
        "Hello over Smart HTTP!\n",
        "Initial HTTP commit",
    );
    stage_and_commit(
        &client_repo,
        "nested/module.rs",
        "pub const MAGIC: usize = 42;\n",
        "Add nested module",
    );

    let push_res = push_to_remote(
        &client_repo,
        &PushOptions {
            remote: Some("origin".to_string()),
            refspecs: vec!["main".to_string()],
            set_upstream: true,
            force: false,
            no_verify: false,
        },
    )
    .unwrap();
    assert_eq!(push_res.updates.len(), 1);

    // 3. Clone from our Smart HTTP server into a fresh directory using `gir_transport::clone_repository`
    let cloned_by_gir = tmp.path().join("cloned-by-gir");
    let _cloned_repo = clone_repository(&base_url, &cloned_by_gir, false).unwrap();
    assert_eq!(
        fs::read_to_string(cloned_by_gir.join("hello.txt")).unwrap(),
        "Hello over Smart HTTP!\n"
    );
    assert_eq!(
        fs::read_to_string(cloned_by_gir.join("nested/module.rs")).unwrap(),
        "pub const MAGIC: usize = 42;\n"
    );

    // 4. Also verify official `/usr/bin/git clone` can clone directly from our `gir-transport` HTTP server!
    run_git(
        tmp.path(),
        &["clone", &base_url, git_client_dir.to_str().unwrap()],
    );
    assert_eq!(
        fs::read_to_string(git_client_dir.join("hello.txt")).unwrap(),
        "Hello over Smart HTTP!\n"
    );
    run_git(&git_client_dir, &["fsck", "--full"]);
}

fn handle_smart_http_connection(mut stream: std::net::TcpStream, repo: &Repository) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
        return;
    }
    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 2 {
        return;
    }
    let method = parts[0];
    let path = parts[1];

    let mut content_len = 0usize;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).unwrap_or(0);
        if n == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        if let Some(val) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            content_len = val.trim().parse().unwrap_or(0);
        }
    }

    let mut body = vec![0u8; content_len];
    if content_len > 0 {
        let _ = reader.read_exact(&mut body);
    }

    let (content_type, resp_body) = if method == "GET"
        && path.ends_with("/info/refs?service=git-upload-pack")
    {
        (
            "application/x-git-upload-pack-advertisement",
            advertise_upload_pack_refs(repo, true).unwrap(),
        )
    } else if method == "GET" && path.ends_with("/info/refs?service=git-receive-pack") {
        (
            "application/x-git-receive-pack-advertisement",
            advertise_receive_pack_refs(repo, true).unwrap(),
        )
    } else if method == "POST" && path.ends_with("/git-upload-pack") {
        (
            "application/x-git-upload-pack-result",
            handle_upload_pack(repo, &body).unwrap(),
        )
    } else if method == "POST" && path.ends_with("/git-receive-pack") {
        (
            "application/x-git-receive-pack-result",
            handle_receive_pack(repo, &body).unwrap(),
        )
    } else {
        let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
        return;
    };

    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        resp_body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(&resp_body);
    let _ = stream.flush();
}
