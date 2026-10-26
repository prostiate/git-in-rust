use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use gir_core::object::blob::Blob;
use gir_core::object::commit::{Commit, Signature};
use gir_core::object::tag::Tag;
use gir_core::object::tree::{FileMode, Tree, TreeEntry};
use gir_core::storage::atomic::LockFile;
use gir_core::{ObjectId, ObjectKind, ParsedObject, Repository};
use tempfile::tempdir;

#[test]
fn differential_init_passes_git_status_and_fsck() {
    let dir = tempdir().unwrap();
    let repo = Repository::init(dir.path(), false).unwrap();
    assert!(repo.git_dir().join("HEAD").is_file());
    assert!(repo.git_dir().join("config").is_file());

    // Official `git status` must succeed with exit code 0
    let status_out = Command::new("git")
        .args(["status"])
        .current_dir(dir.path())
        .output()
        .expect("run git status");
    assert!(
        status_out.status.success(),
        "git status failed: {}",
        String::from_utf8_lossy(&status_out.stderr)
    );

    // Official `git fsck --full --strict` must pass with exit code 0
    let fsck_out = Command::new("git")
        .args(["fsck", "--full", "--strict"])
        .current_dir(dir.path())
        .output()
        .expect("run git fsck");
    assert!(
        fsck_out.status.success(),
        "git fsck failed: {}",
        String::from_utf8_lossy(&fsck_out.stderr)
    );
}

#[test]
fn differential_hash_object_and_cat_file_interoperability() {
    let dir = tempdir().unwrap();
    let repo = Repository::init(dir.path(), false).unwrap();

    let payload = b"Hello from gir Milestone 1!\n\x00\xffBinary test payload.\n";
    let file_path = dir.path().join("sample.bin");
    fs::write(&file_path, payload).unwrap();

    // Write blob via gir
    let gir_oid = repo
        .odb()
        .write_object(&Blob::new(payload.to_vec()).to_raw())
        .unwrap();

    // Compute hash via official `git hash-object`
    let git_hash_out = Command::new("git")
        .args(["hash-object", file_path.to_str().unwrap()])
        .current_dir(dir.path())
        .output()
        .expect("git hash-object");
    assert!(git_hash_out.status.success());
    let git_hex = String::from_utf8(git_hash_out.stdout).unwrap();
    assert_eq!(gir_oid.to_hex(), git_hex.trim());

    // Read the object written by `gir` using official `git cat-file -p`
    let git_cat_out = Command::new("git")
        .args(["cat-file", "-p", &gir_oid.to_hex()])
        .current_dir(dir.path())
        .output()
        .expect("git cat-file -p");
    assert!(git_cat_out.status.success());
    assert_eq!(git_cat_out.stdout.as_slice(), payload);

    // Also test the reverse: write an object with `git hash-object -w` and read it with `gir`
    let git_written_payload = b"Object written by official git CLI\n";
    let file2 = dir.path().join("from_git.txt");
    fs::write(&file2, git_written_payload).unwrap();

    let git_write_out = Command::new("git")
        .args(["hash-object", "-w", file2.to_str().unwrap()])
        .current_dir(dir.path())
        .output()
        .expect("git hash-object -w");
    let git_oid = ObjectId::from_hex(std::str::from_utf8(&git_write_out.stdout).unwrap()).unwrap();

    // Resolve via short 7-char prefix in gir and verify payload
    let resolved = repo.resolve_object(&git_oid.short_hex()).unwrap();
    assert_eq!(resolved, git_oid);
    let raw = repo.odb().read_raw(&resolved).unwrap();
    assert_eq!(raw.kind, ObjectKind::Blob);
    assert_eq!(raw.data, git_written_payload);
}

#[test]
fn differential_tree_sorting_and_commit_matches_git_byte_for_byte() {
    let dir = tempdir().unwrap();
    let repo = Repository::init(dir.path(), false).unwrap();

    let blob_oid = repo
        .odb()
        .write_raw(ObjectKind::Blob, b"content\n")
        .unwrap();
    let sub_tree = Tree::new(vec![TreeEntry::new(FileMode::Regular, "inner.rs", blob_oid)]);
    let sub_tree_oid = repo.odb().write_object(&sub_tree.to_raw()).unwrap();

    // Tricky directory-aware sorting case: "foo-bar", "foo.txt", "foo" (dir), "foo0"
    let root_tree = Tree::new(vec![
        TreeEntry::new(FileMode::Directory, "foo", sub_tree_oid),
        TreeEntry::new(FileMode::Regular, "foo.txt", blob_oid),
        TreeEntry::new(FileMode::Executable, "foo-bar", blob_oid),
        TreeEntry::new(FileMode::Regular, "foo0", blob_oid),
    ]);
    let gir_tree_oid = repo.odb().write_object(&root_tree.to_raw()).unwrap();

    // Build the exact same tree using official `git mktree`
    let mktree_input = format!(
        "100755 blob {blob_oid}\tfoo-bar\n\
         100644 blob {blob_oid}\tfoo.txt\n\
         040000 tree {sub_tree_oid}\tfoo\n\
         100644 blob {blob_oid}\tfoo0\n"
    );
    let mut child = Command::new("git")
        .args(["mktree"])
        .current_dir(dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn git mktree");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(mktree_input.as_bytes())
        .unwrap();
    let mktree_out = child.wait_with_output().unwrap();
    assert!(mktree_out.status.success());
    let git_tree_hex = String::from_utf8(mktree_out.stdout).unwrap();
    assert_eq!(gir_tree_oid.to_hex(), git_tree_hex.trim());

    // Now create a Commit and an Annotated Tag in gir and verify with `git fsck --full --strict`
    let sig = Signature::new(
        "Muhammad Irfan Kurniawan",
        "mail@irfankurniawan.com",
        1730000000,
        "+0700",
    );
    let commit = Commit {
        tree: gir_tree_oid,
        parents: vec![],
        author: sig.clone(),
        committer: sig.clone(),
        message: "feat: milestone 1 tree and commit test\n".to_string(),
    };
    let commit_oid = repo.odb().write_object(&commit.to_raw()).unwrap();

    let tag = Tag {
        object: commit_oid,
        object_kind: ObjectKind::Commit,
        tag_name: "v0.1.0".to_string(),
        tagger: Some(sig),
        message: "Release v0.1.0\n".to_string(),
    };
    let tag_oid = repo.odb().write_object(&tag.to_raw()).unwrap();

    match repo.odb().read_parsed(&tag_oid).unwrap() {
        ParsedObject::Tag(parsed_tag) => assert_eq!(parsed_tag, tag),
        other => panic!("expected Tag, got {other:?}"),
    }

    let fsck_out = Command::new("git")
        .args(["fsck", "--full", "--strict"])
        .current_dir(dir.path())
        .output()
        .expect("run git fsck");
    assert!(
        fsck_out.status.success(),
        "git fsck failed: {}",
        String::from_utf8_lossy(&fsck_out.stderr)
    );
}

#[test]
fn lockfile_concurrent_backoff_waits_for_short_holder() {
    let dir = tempdir().unwrap();
    let target = dir.path().join("index");

    let lock1 = LockFile::acquire(&target).unwrap();
    let target_clone = target.clone();

    // Release lock1 after 15ms on a background thread
    let releaser = thread::spawn(move || {
        thread::sleep(Duration::from_millis(15));
        drop(lock1);
    });

    // Main thread acquires lock with 100ms default backoff; must succeed!
    let mut lock2 = LockFile::acquire(&target_clone).expect("should succeed after 15ms release");
    lock2.write_all(b"updated").unwrap();
    lock2.commit().unwrap();
    releaser.join().unwrap();

    assert_eq!(fs::read(&target).unwrap(), b"updated");
}
