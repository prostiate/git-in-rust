use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};

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
fn cli_pack_index_unpack_gc_and_fsck_workflow() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    // 1. Initialize repo and create commits with git
    run_git(root, &["init", "-b", "main"]);
    fs::write(root.join("a.txt"), "alpha content\n".repeat(30)).unwrap();
    run_git(root, &["add", "a.txt"]);
    run_git(root, &["commit", "-m", "first"]);

    fs::write(root.join("a.txt"), "alpha content\nbeta suffix\n".repeat(30)).unwrap();
    run_git(root, &["add", "a.txt"]);
    run_git(root, &["commit", "-m", "second"]);

    // 2. Run `gir pack-objects --all --stdout` to produce a pack stream
    let pack_out = Command::new(gir_bin())
        .current_dir(root)
        .args(["pack-objects", "--all", "--stdout"])
        .output()
        .unwrap();
    assert!(pack_out.status.success());
    let pack_bytes = pack_out.stdout;
    assert!(pack_bytes.starts_with(b"PACK"));

    // 3. Write pack stream to disk and run `gir index-pack`
    let ext_pack_path = root.join("bundle.pack");
    fs::write(&ext_pack_path, &pack_bytes).unwrap();
    let idx_out = Command::new(gir_bin())
        .current_dir(root)
        .args(["index-pack", ext_pack_path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(idx_out.status.success());
    assert!(root.join("bundle.idx").is_file());

    // Verify official git accepts the generated bundle.idx
    run_git(root, &["verify-pack", "-v", root.join("bundle.idx").to_str().unwrap()]);
    fs::remove_file(root.join("bundle.pack")).unwrap();
    fs::remove_file(root.join("bundle.idx")).unwrap();

    // 4. Test `gir unpack-objects` into a brand-new `gir init` repository
    let dest_tmp = TempDir::new().unwrap();
    let dest = dest_tmp.path();
    let init_out = Command::new(gir_bin())
        .current_dir(dest)
        .args(["init"])
        .output()
        .unwrap();
    assert!(init_out.status.success());

    let mut unpack_child = Command::new(gir_bin())
        .current_dir(dest)
        .args(["unpack-objects", "-q"])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    unpack_child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(&pack_bytes)
        .unwrap();
    let unpack_status = unpack_child.wait().unwrap();
    assert!(unpack_status.success());

    // 5. Run `gir gc -q` and `gir fsck --full` on the original repository
    let gc_out = Command::new(gir_bin())
        .current_dir(root)
        .args(["gc", "-q"])
        .output()
        .unwrap();
    assert!(gc_out.status.success());

    let fsck_out = Command::new(gir_bin())
        .current_dir(root)
        .args(["fsck", "--full"])
        .output()
        .unwrap();
    assert!(
        fsck_out.status.success(),
        "gir fsck failed: {}",
        String::from_utf8_lossy(&fsck_out.stderr)
    );

    // And verify official `git fsck --full` also passes
    run_git(root, &["fsck", "--full"]);
}
