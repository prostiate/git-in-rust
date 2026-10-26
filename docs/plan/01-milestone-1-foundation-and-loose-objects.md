# Milestone 1: Workspace Foundation, Atomic Storage, `.git/config` & Loose Object Plumbing

**Parent Index**: [`00-index.md`](./00-index.md)  
**Target Crates**: `gir-core`, `gir-cli`  
**Commands Delivered**: `gir init`, `gir config`, `gir hash-object`, `gir cat-file`, `gir ls-tree`  

---

## 1. What You Will Learn in This Milestone

### Systems Engineering Concepts
- **Content-Addressable Storage (CAS)**: How Git turns arbitrary files, directories, and commit metadata into deterministic 20-byte SHA-1 hashes via `<type> <ascii-len>\0<payload>`.
- **POSIX Atomic Persistence**: Why naive `std::fs::write()` corrupts repositories on power loss or concurrent writes, and how `O_CREAT | O_EXCL` lockfiles + `fsync` + atomic `rename` + parent-directory `fsync` guarantee hardware durability on Linux (`ext4`/`xfs`).
- **Binary vs. Text Object Formats**: Why Git stores commit/tag objects as UTF-8 text headers, but stores `tree` object entries using raw 20-byte binary SHA-1s (`<octal-mode> <name>\0<20-byte-raw-sha>`).

### Rust Concepts
- **Cargo Workspaces**: Sharing dependencies (`[workspace.dependencies]`) across multiple crates (`gir-core`, `gir-transport`, `gir-tui`, `gir-cli`).
- **Stack-Allocated Newtypes**: Wrapping `[u8; 20]` inside `#[repr(transparent)] pub struct ObjectId([u8; 20])` with `Display`, `FromStr`, `Ord`, and `Hash` without heap allocations.
- **RAII & the `Drop` Trait**: Designing `LockFile` so that if any function returns `Err(...)` or panics before `.commit()`, the `.lock` file is automatically unlinked from disk.
- **Streaming I/O Teeing**: Computing a SHA-1 hash while simultaneously zlib-encoding bytes to a temporary file in a single streaming pass (`Read` / `Write` composition).

---

## 2. Files to Create

```text
git-in-rust/
├── Cargo.toml
└── crates/
    ├── gir-core/
    │   ├── Cargo.toml
    │   ├── src/
    │   │   ├── lib.rs
    │   │   ├── error.rs            # GirError enum (thiserror)
    │   │   ├── hash.rs             # ObjectId([u8; 20]), Hex parsing/formatting
    │   │   ├── config.rs           # .git/config & ~/.gitconfig INI parser/writer
    │   │   ├── repository.rs       # Repository discovery (.git walk-up) & gir init
    │   │   ├── storage/
    │   │   │   ├── mod.rs
    │   │   │   ├── atomic.rs       # LockFile, AtomicTempFile, fsync_dir
    │   │   │   └── loose.rs        # LooseObjectStore (.git/objects/xx/yyyy...)
    │   │   └── object/
    │   │       ├── mod.rs          # ObjectKind (Blob, Tree, Commit, Tag), RawObject
    │   │       ├── blob.rs         # Blob struct
    │   │       ├── tree.rs         # Tree & TreeEntry (FileMode, name, ObjectId)
    │   │       ├── commit.rs       # Commit & Signature (name, email, timestamp, tz)
    │   │       └── tag.rs          # Annotated Tag struct
    │   └── tests/
    │       └── milestone_1_differential.rs
    ├── gir-transport/
    │   ├── Cargo.toml
    │   └── src/lib.rs
    ├── gir-tui/
    │   ├── Cargo.toml
    │   └── src/lib.rs
    └── gir-cli/
        ├── Cargo.toml
        └── src/
            ├── main.rs
            └── commands/
                ├── mod.rs
                ├── init.rs
                ├── config.rs
                ├── hash_object.rs
                ├── cat_file.rs
                └── ls_tree.rs
```

---

## 3. Byte-Level Specifications

### 3.1 Loose Object Envelope
Every loose object before zlib compression has the exact byte layout:
```text
<ascii-type> + 0x20 (' ') + <ascii-decimal-byte-length> + 0x00 ('\0') + <payload-bytes>
```
- `ascii-type`: `"blob"`, `"tree"`, `"commit"`, or `"tag"`.
- `ObjectId`: `SHA1(header + payload)`.
- On-disk path: `.git/objects/<hex[0..2]>/<hex[2..40]>`, permissions `0444` (read-only once written).

### 3.2 `tree` Object Payload Layout
Unlike commits, `tree` payloads contain **raw 20-byte SHA-1s**, sorted by Git's special tree sort order (where directories are sorted as if their name had a trailing `/`):
```text
<ascii-octal-mode-without-leading-zero> + 0x20 (' ') + <entry-name-bytes> + 0x00 ('\0') + <20-byte-raw-sha1>
```
Valid modes:
- `100644` (normal regular file — note: **no** leading `0`, unlike `.git/index`)
- `100755` (executable regular file)
- `120000` (symbolic link)
- `40000` (subdirectory tree — note: **5 characters** `"40000"`, no leading `0`!)
- `160000` (submodule gitlink)

### 3.3 `commit` Object Payload Layout
```text
tree <40-hex-sha>\n
parent <40-hex-sha>\n        (0, 1, or more parent lines)
author <Name> <<Email>> <unix-seconds> <+HHMM|-HHMM>\n
committer <Name> <<Email>> <unix-seconds> <+HHMM|-HHMM>\n
\n
<commit message UTF-8>
```

---

## 4. Step-by-Step Implementation Tasks

### Task 1.1: Workspace Scaffolding & `ObjectId` (`hash.rs`)
1. Create root `Cargo.toml` with workspace members `crates/gir-core`, `crates/gir-transport`, `crates/gir-tui`, `crates/gir-cli`.
2. Implement `ObjectId([u8; 20])`:
   - `ObjectId::from_hex(s: &str) -> Result<Self, GirError>` (strictly 40 hex chars, lowercase/uppercase).
   - `ObjectId::to_hex(&self) -> String` (40 lowercase hex chars).
   - `ObjectId::as_bytes(&self) -> &[u8; 20]`.
   - `ObjectId::fanout_prefix(&self) -> (String, String)` returning `("ab", "cdef...")`.
3. Write unit tests verifying round-trip hex conversion and invalid input rejection.

### Task 1.2: Atomic Persistence & `LockFile` (`storage/atomic.rs`)
1. Implement `fsync_dir(dir: &Path) -> io::Result<()>`: opens directory as a `File` and calls `.sync_all()`.
2. Implement `LockFile`:
   - `LockFile::acquire(target_path: impl AsRef<Path>) -> Result<Self, GirError>` and `LockFile::acquire_with_timeout(target_path, timeout: Duration)`: creates `<target_path>.lock` using `OpenOptions::new().write(true).create_new(true)` (`O_CREAT | O_EXCL`).
   - **Concurrent `git` / IDE Backoff**: By default, retries with quadratic backoff (`1ms, 4ms, 9ms, 16ms, 25ms, 50ms`, up to `100ms` total) if `ErrorKind::AlreadyExists`, so background `git status` processes spawned by VS Code, Cursor, or `OnoGitTree` never cause annoying lock failures when `git` and `gir` run at the exact same millisecond. If still locked after timeout, returns `GirError::LockContention(path)`.
   - Implements `std::io::Write` via an internal `BufWriter<File>`.
   - `LockFile::commit(mut self) -> Result<(), GirError>`: flushes `BufWriter`, calls `file.sync_all()`, atomically `std::fs::rename(&self.lock_path, &self.target_path)`, calls `fsync_dir` on parent directory, and marks `committed = true`.
   - `impl Drop for LockFile`: if `!self.committed`, `let _ = std::fs::remove_file(&self.lock_path);`.
3. Implement `AtomicTempFile` for `.git/objects/xx/tmp_obj_XXXXXX`:
   - Writes to temp file in `.git/objects/xx/`, `sync_all()`, sets read-only permissions (`0444`), renames to `.git/objects/xx/yyyy...` (ignoring `EEXIST` since content is immutable and content-addressed), and `fsync_dir`.

### Task 1.3: Repository Initialization, Discovery & Config (`repository.rs` & `config.rs`)
1. Implement `Repository::init(path: &Path, bare: bool) -> Result<Repository, GirError>`:
   - Creates `.git/` (or directly in `path` if `--bare`), along with `objects/info`, `objects/pack`, `refs/heads`, `refs/tags`, `logs/refs/heads`.
   - Atomically writes `.git/HEAD` with `ref: refs/heads/main\n` (respecting `init.defaultBranch` in `~/.gitconfig` if present).
   - Atomically writes default `.git/config` (`repositoryformatversion = 0`, `filemode = true`, `bare = false`, `logallrefupdates = true`).
2. Implement `Repository::discover(start_dir: &Path) -> Result<Repository, GirError>`:
   - Checks `GIT_DIR` environment variable first.
   - Walks up parent directories until a valid `.git` directory is found (stopping at filesystem boundary or `GIT_CEILING_DIRECTORIES`).
3. Implement `GitConfig` parser/writer (`config.rs`):
   - Supports `[section]` and `[section "subsection"]` headers, `key = value` pairs, `#` and `;` comments, and cascading resolution (`system` $\rightarrow$ `global ~/.gitconfig` $\rightarrow$ `local .git/config`).
   - Helper `Repository::resolve_signature() -> Result<Signature, GirError>` checking `GIT_AUTHOR_NAME` / `GIT_AUTHOR_EMAIL` env vars, then `user.name` / `user.email` in `GitConfig`.

### Task 1.4: Object Models & Loose Object Store (`object/*` & `storage/loose.rs`)
1. Implement `ObjectKind` (`Blob`, `Tree`, `Commit`, `Tag`) and `RawObject { kind: ObjectKind, data: Vec<u8> }`.
2. Implement `Blob`, `Tree` (with Git's directory-aware comparator: `a` vs `a/` sorting rule!), `Commit`, and `Tag` binary parsers and serializers.
3. Implement `LooseObjectStore`:
   - `write_raw(&self, kind: ObjectKind, payload: &[u8]) -> Result<ObjectId, GirError>`: computes SHA-1 of header + payload, checks if `.git/objects/xx/yyyy...` already exists, and if not, zlib-compresses into `AtomicTempFile` and commits.
   - `read_raw(&self, id: &ObjectId) -> Result<RawObject, GirError>`: opens loose object, streams through `flate2::read::ZlibDecoder`, parses `<kind> <size>\0`, verifies `payload.len() == size` and SHA-1 integrity.
   - `resolve_prefix(&self, hex_prefix: &str) -> Result<Vec<ObjectId>, GirError>`: scans `.git/objects/<prefix[0..2]>/` for matching loose objects (minimum 4 hex chars).

### Task 1.5: CLI Plumbing Commands (`gir-cli`)
1. `gir init [--bare] [path]`
2. `gir config [--global | --local] [--get | --list | <key> <value>]`
3. `gir hash-object [-t <type>] [-w] [--stdin | <file>...]`
4. `gir cat-file (-p | -t | -s | -e) <object>`
5. `gir ls-tree [-r] [-d] [--name-only] <tree-ish>`

---

## 5. Differential Verification Suite (`milestone_1_differential.rs`)

1. **`init` Interop Test**: Run `gir init` in a temp dir, then run `/usr/bin/git status` and `/usr/bin/git fsck --full` inside it. Assert exit code `0`.
2. **`hash-object` Byte-Identity Test**: Generate random binary/text files, write them with `gir hash-object -w`, and verify:
   - Returned SHA-1 matches `/usr/bin/git hash-object <file>`.
   - `/usr/bin/git cat-file -p <sha>` outputs the exact original bytes.
3. **`Tree` Sorting Edge-Case Test**: Create a tree containing files `"foo"`, `"foo-bar"`, `"foo.txt"`, and directory `"foo"`. Verify `Tree::serialize()` sorts entries identically to `/usr/bin/git mktree` (`"foo-bar"` < `"foo.txt"` < `"foo/"`).
4. **Lockfile RAII Drop Test**: Acquire `LockFile` on `.git/config`, simulate an early error return before `.commit()`, and assert `.git/config.lock` is removed automatically and `.git/config` is untouched.
