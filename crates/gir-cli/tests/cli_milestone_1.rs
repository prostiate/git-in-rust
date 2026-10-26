use std::fs;
use std::process::Command;

use gir_core::object::tree::{FileMode, Tree, TreeEntry};
use gir_core::{ObjectId, Repository};
use tempfile::tempdir;

fn gir_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gir")
}

#[test]
fn cli_init_config_hash_cat_and_ls_tree_end_to_end() {
    let dir = tempdir().unwrap();

    // 1. gir init
    let init_out = Command::new(gir_bin())
        .args(["init"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(init_out.status.success());
    assert!(String::from_utf8_lossy(&init_out.stdout).contains("Initialized empty Git repository"));

    // 2. gir config --local
    let cfg_set = Command::new(gir_bin())
        .args(["config", "--local", "user.name", "Muhammad Irfan Kurniawan"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(cfg_set.status.success());

    let cfg_get = Command::new(gir_bin())
        .args(["config", "--local", "--get", "user.name"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(cfg_get.status.success());
    assert_eq!(
        String::from_utf8_lossy(&cfg_get.stdout).trim(),
        "Muhammad Irfan Kurniawan"
    );

    // 3. gir hash-object -w
    let hello_file = dir.path().join("hello.txt");
    fs::write(&hello_file, b"Hello Rust Git!\n").unwrap();

    let hash_out = Command::new(gir_bin())
        .args(["hash-object", "-w", "hello.txt"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(hash_out.status.success());
    let blob_sha = String::from_utf8_lossy(&hash_out.stdout).trim().to_string();
    assert_eq!(blob_sha.len(), 40);

    // 4. gir cat-file -t, -s, -p
    let type_out = Command::new(gir_bin())
        .args(["cat-file", "-t", &blob_sha[..7]])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&type_out.stdout).trim(), "blob");

    let size_out = Command::new(gir_bin())
        .args(["cat-file", "-s", &blob_sha])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&size_out.stdout).trim(), "16");

    let pretty_out = Command::new(gir_bin())
        .args(["cat-file", "-p", &blob_sha])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert_eq!(&pretty_out.stdout, b"Hello Rust Git!\n");

    // 5. Write a nested tree and test `gir ls-tree -r` against `/usr/bin/git ls-tree -r`
    let repo = Repository::discover(dir.path()).unwrap();
    let blob_oid = ObjectId::from_hex(&blob_sha).unwrap();
    let sub_tree = Tree::new(vec![TreeEntry::new(FileMode::Regular, "lib.rs", blob_oid)]);
    let sub_tree_oid = repo.odb().write_object(&sub_tree.to_raw()).unwrap();

    let root_tree = Tree::new(vec![
        TreeEntry::new(FileMode::Regular, "README.md", blob_oid),
        TreeEntry::new(FileMode::Directory, "src", sub_tree_oid),
    ]);
    let root_tree_oid = repo.odb().write_object(&root_tree.to_raw()).unwrap();

    let gir_lstree = Command::new(gir_bin())
        .args(["ls-tree", "-r", &root_tree_oid.to_hex()])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let git_lstree = Command::new("git")
        .args(["ls-tree", "-r", &root_tree_oid.to_hex()])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(gir_lstree.status.success());
    assert_eq!(gir_lstree.stdout, git_lstree.stdout);
}
