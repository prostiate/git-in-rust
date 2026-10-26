use std::fs;
use std::process::Command;

use gir_core::object::ParsedObject;
use gir_core::pack::{index_pack_file, write_pack_and_index};
use gir_core::storage::fsck::{fsck_repository, gc_repository};
use gir_core::{ObjectId, Repository};
use tempfile::TempDir;

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
        .expect("failed to execute official git binary");
    assert!(
        out.status.success(),
        "official `git {:?}` failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

#[test]
fn real_packed_repo_aggressive_gc_differential() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    run_git(root, &["init", "-b", "main"]);
    fs::create_dir_all(root.join("src")).unwrap();

    // Create 15 commits evolving source files so `git gc --aggressive` builds deep OFS_DELTA chains
    let mut commit_shas = Vec::new();
    for i in 1..=15 {
        let mut content = String::new();
        for line in 0..120 {
            content.push_str(&format!(
                "pub fn helper_{line}() -> usize {{ {} + {line} }}\n",
                i * 10
            ));
        }
        fs::write(root.join("src/lib.rs"), &content).unwrap();
        fs::write(root.join("README.md"), format!("# Release v1.{i}\n")).unwrap();
        run_git(root, &["add", "."]);
        run_git(root, &["commit", "-m", &format!("commit #{i}")]);

        let rev = run_git(root, &["rev-parse", "HEAD"]);
        commit_shas.push(String::from_utf8(rev.stdout).unwrap().trim().to_string());
    }

    // Create an annotated tag as well
    run_git(root, &["tag", "-a", "v1.15", "-m", "tagged release v1.15"]);

    // Pack 100% of objects with aggressive delta chains
    run_git(root, &["gc", "--aggressive", "--prune=now"]);

    let repo = Repository::discover(root).unwrap();
    assert_eq!(
        repo.odb().loose().list_all().unwrap().len(),
        0,
        "all objects must be inside .pack files after git gc --aggressive"
    );
    assert_eq!(repo.odb().refresh_packs().len(), 1);

    // Verify `gir` reads every commit, tree, and blob identically to `/usr/bin/git`
    let all_oids = repo.odb().list_all_objects().unwrap();
    assert!(all_oids.len() >= 45);

    for oid in &all_oids {
        let raw = repo.odb().read_raw(oid).unwrap();
        let git_cat = run_git(root, &["cat-file", raw.kind.as_str(), &oid.to_hex()]);
        assert_eq!(
            raw.data, git_cat.stdout,
            "packed object payload mismatch for {oid} ({})",
            raw.kind
        );

        // Also test short-SHA prefix resolution across `.idx` v2 fanout table
        let prefix = &oid.to_hex()[..8];
        let resolved = repo.odb().resolve_prefix(prefix).unwrap();
        assert_eq!(&resolved, oid);
    }

    // Verify `gir` resolves packed refs (`HEAD`, `main`, `v1.15`) after `git gc`
    let head_oid = repo.resolve_object("HEAD").unwrap();
    assert_eq!(head_oid.to_hex(), *commit_shas.last().unwrap());
    let main_oid = repo.resolve_object("main").unwrap();
    assert_eq!(main_oid, head_oid);
    let tag_oid = repo.resolve_object("v1.15").unwrap();
    let ParsedObject::Tag(tag) = repo.odb().read_parsed(&tag_oid).unwrap() else {
        panic!("expected annotated tag");
    };
    assert_eq!(tag.object, head_oid);

    // Verify `fsck_repository` passes 100% of pack checksums, CRC32s, and DAG links
    let report = fsck_repository(&repo, true).unwrap();
    assert!(report.is_ok(), "fsck errors: {:?}", report.errors);
    assert_eq!(report.checked_objects, all_oids.len());
}

#[test]
fn gir_pack_objects_verified_by_official_git_verify_pack() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    let repo = Repository::init(root, false).unwrap();
    let mut objects = Vec::new();

    // Write 10 versions of a large blob so `gir` emits OBJ_OFS_DELTA entries
    for v in 0..10 {
        let mut text = String::new();
        for line in 0..200 {
            text.push_str(&format!("fn compute_{line}() -> u64 {{ {line} + {v} }}\n"));
        }
        let oid = repo
            .odb()
            .write_raw(gir_core::ObjectKind::Blob, text.as_bytes())
            .unwrap();
        let raw = repo.odb().read_raw(&oid).unwrap();
        objects.push((oid, raw));
    }

    let pack_path = root.join("test-out.pack");
    let idx_path = root.join("test-out.idx");
    write_pack_and_index(&objects, &pack_path, &idx_path).unwrap();

    // Official `/usr/bin/git verify-pack -v` validates headers, SHA-1 trailers, CRC32s, and delta chains
    let verify_out = run_git(root, &["verify-pack", "-v", idx_path.to_str().unwrap()]);
    let verify_stdout = String::from_utf8(verify_out.stdout).unwrap();
    // Confirm that delta compression was actually used (chain length >= 1 present)
    assert!(
        verify_stdout.contains("chain length = "),
        "verify-pack output should include delta chain statistics:\n{verify_stdout}"
    );
}

#[test]
fn gir_index_pack_byte_for_byte_identical_to_official_git() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    run_git(root, &["init", "-b", "main"]);
    for i in 0..8 {
        fs::write(
            root.join(format!("file_{i}.txt")),
            format!("content version {i} with repeated text for delta compression\n").repeat(30),
        )
        .unwrap();
        run_git(root, &["add", "."]);
        run_git(root, &["commit", "-m", &format!("c{i}")]);
    }

    // Repack via official git
    run_git(root, &["repack", "-a", "-d"]);

    let pack_dir = root.join(".git/objects/pack");
    let git_idx_path = fs::read_dir(&pack_dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.extension().and_then(|s| s.to_str()) == Some("idx"))
        .expect("git repack should produce an .idx file");
    let git_pack_path = git_idx_path.with_extension("pack");

    // Copy the `.pack` file to a fresh path and build its `.idx` using `gir`'s `index_pack_file`
    let gir_pack_copy = root.join("gir-copy.pack");
    let gir_idx_out = root.join("gir-copy.idx");
    fs::copy(&git_pack_path, &gir_pack_copy).unwrap();

    let (_, pack_sha, _) =
        index_pack_file(&gir_pack_copy, Some(&gir_idx_out), None).unwrap();
    assert!(git_pack_path.to_string_lossy().contains(&pack_sha.to_hex()));

    let official_idx_bytes = fs::read(&git_idx_path).unwrap();
    let gir_idx_bytes = fs::read(&gir_idx_out).unwrap();
    assert_eq!(
        gir_idx_bytes, official_idx_bytes,
        "gir index-pack .idx v2 output must be 100% byte-for-byte identical to official git index-pack"
    );
}

#[test]
fn gir_gc_verified_by_official_git_fsck_and_log() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    run_git(root, &["init", "-b", "main"]);
    for i in 1..=6 {
        fs::write(
            root.join("app.rs"),
            format!("fn main() {{ println!(\"step {i}\"); }}\n").repeat(40),
        )
        .unwrap();
        run_git(root, &["add", "app.rs"]);
        run_git(root, &["commit", "-m", &format!("step {i}")]);
    }

    let repo = Repository::discover(root).unwrap();
    let loose_before = repo.odb().loose().list_all().unwrap().len();
    assert!(loose_before >= 12);

    let gc_report = gc_repository(&repo, false).unwrap();
    assert_eq!(gc_report.packed_objects, loose_before);
    assert_eq!(gc_report.removed_loose_objects, loose_before);
    assert_eq!(repo.odb().loose().list_all().unwrap().len(), 0);

    // Official `/usr/bin/git fsck --full` and `git log` must succeed on the gir-gc'd repository
    run_git(root, &["fsck", "--full", "--strict"]);
    let log_out = run_git(root, &["log", "--oneline"]);
    let log_str = String::from_utf8(log_out.stdout).unwrap();
    assert_eq!(log_str.lines().count(), 6);

    // And `gir` itself must continue resolving `HEAD` and reading objects from the new pack
    let head_oid: ObjectId = repo.resolve_object("HEAD").unwrap();
    let head_obj = repo.odb().read_parsed(&head_oid).unwrap();
    assert!(matches!(head_obj, ParsedObject::Commit(_)));
}
