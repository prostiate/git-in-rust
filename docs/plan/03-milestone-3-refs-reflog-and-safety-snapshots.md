# Milestone 3: Reference Manager, Reflog, Revision Parser & Safety Snapshots (`gir undo`)

**Parent Index**: [`00-index.md`](./00-index.md)  
**Target Crates**: `gir-core`, `gir-cli`  
**Commands Delivered**: `gir update-ref`, `gir symbolic-ref`, `gir rev-parse`, `gir reflog`, `gir undo`  

---

## 1. What You Will Learn in This Milestone

### Systems Engineering Concepts
- **Hierarchical & Packed Reference Resolution**: How Git resolves a human name like `main` or `origin/main` across loose files (`.git/refs/...`) and the consolidated `.git/packed-refs` file, using compare-and-swap (CAS) old-value checks inside `.lock` files to prevent lost updates.
- **Append-Only Audit Journals (Reflog)**: How `.git/logs/HEAD` and `.git/logs/refs/heads/*` record every state transition so detached commits remain reachable and recoverable.
- **Content-Addressable Pre-Operation Safety Snapshots (`gir undo`)**: How to design a zero-overhead safety net on top of Git's object model so even destructive operations (`reset --hard`, dirty `checkout`, botched `merge`) never lose uncommitted work.
- **Revision Expression Grammar**: How Git parses complex expressions like `HEAD~3^2`, `main@{1}`, `v1.0^{commit}`, and `main..feature` into concrete `ObjectId`s.

### Rust Concepts
- **Compare-And-Swap (CAS) Transaction Guards**: Enforcing `ExpectedOldTarget` verification while holding a `LockFile` before mutating a reference.
- **Zero-Allocation String Combinator Parsing**: Parsing recursive revision suffixes (`~N`, `^N`, `@{N}`, `^{type}`) over `&str` slices cleanly.

---

## 2. Files to Create / Modify

```text
crates/gir-core/src/
├── refs/
│   ├── mod.rs              # RefStore (loose + packed-refs unified API)
│   ├── packed.rs           # .git/packed-refs parser & atomic writer
│   └── transaction.rs      # CAS RefTransaction (old_oid -> new_oid + reflog)
├── reflog/
│   ├── mod.rs              # ReflogEntry, read_reflog, append_reflog
│   └── snapshot.rs         # SafetySnapshotEngine (refs/gir/snapshots/* & gir undo)
└── revision/
    └── mod.rs              # parse_revision(repo, spec) & RevisionRange
crates/gir-cli/src/commands/
├── update_ref.rs
├── symbolic_ref.rs
├── rev_parse.rs
├── reflog.rs
└── undo.rs
```

---

## 3. Byte-Level & Format Specifications

### 3.1 Reference Resolution Precedence Rules (Git `dwim` Order)
When resolving an unqualified ref name `<name>`:
1. `.git/<name>` (if `<name>` is `HEAD`, `FETCH_HEAD`, `ORIG_HEAD`, `MERGE_HEAD`, `CHERRY_PICK_HEAD`)
2. `.git/refs/<name>`
3. `.git/refs/tags/<name>`
4. `.git/refs/heads/<name>`
5. `.git/refs/remotes/<name>`
6. `.git/refs/remotes/<name>/HEAD`
Loose ref files always take precedence over entries in `.git/packed-refs`.

### 3.2 `.git/packed-refs` Format
```text
# pack-refs with: peeled fully-peeled sorted 
<40-hex-sha> refs/heads/main\n
<40-hex-sha> refs/tags/v1.0\n
^<40-hex-peeled-target-sha>\n
```
- Lines starting with `#` are headers/comments.
- Lines starting with `^` immediately follow an annotated tag line and specify the peeled target `ObjectId`.

### 3.3 `.git/logs/<ref>` Reflog Format
Every line in `.git/logs/HEAD` or `.git/logs/refs/heads/<branch>` has the exact format:
```text
<40-hex-old-sha> <40-hex-new-sha> <Name> <<Email>> <unix-seconds> <+HHMM|-HHMM>\t<action>: <message>\n
```
- When a ref is created for the first time, `<40-hex-old-sha>` is `0000000000000000000000000000000000000000` (`ObjectId::ZERO`).

### 3.4 Pre-Operation Safety Snapshot Format (`reflog/snapshot.rs`)
Before any destructive command (`reset`, `checkout`, `restore`, `clean`, `merge`, `rebase`):
1. If working tree or index has changes relative to `HEAD`, `SafetySnapshotEngine::capture(repo, op_name)`:
   - Writes any modified/untracked working-tree files into the loose object store as `Blob`s and builds a working-tree `Tree` object (`wt_tree`).
   - Writes the current `.git/index` state into a `Tree` object (`idx_tree`).
   - Writes a standard `Commit` object whose:
     - `tree` = `wt_tree`
     - `parent[0]` = current `HEAD` commit (if any)
     - `parent[1]` = a commit wrapping `idx_tree`
     - `message` = `"gir-snapshot: <op_name> on <branch> at <iso-timestamp>\nHEAD-ref: <symbolic-or-detached-head>"`
   - Updates `.git/refs/gir/snapshots/latest` and appends to `.git/logs/refs/gir/snapshots/history`.
2. Because this uses 100% standard Git `Commit`/`Tree`/`Blob` objects under `refs/gir/snapshots/*`, `git fsck` considers all snapshot objects valid and reachable!

---

## 4. Step-by-Step Implementation Tasks

### Task 3.1: `packed-refs` & Loose `RefStore` (`refs/mod.rs` & `refs/packed.rs`)
1. Implement `RefTarget`: `Direct(ObjectId)` or `Symbolic(String)` (`ref: refs/heads/...`).
2. Implement `PackedRefs::load(git_dir: &Path) -> Result<Self, GirError>` and atomic `PackedRefs::write()`.
3. Implement `RefStore`:
   - `resolve_raw(&self, ref_name: &str) -> Result<Option<RefTarget>, GirError>`
   - `resolve_to_oid(&self, name: &str) -> Result<ObjectId, GirError>` (follows symbolic ref chains up to max depth 10).
   - `update_ref(&self, ref_name: &str, new_oid: ObjectId, expected_old: Option<ObjectId>, sig: &Signature, reflog_msg: &str) -> Result<(), GirError>`:
     - Acquires `LockFile` on `.git/<ref_name>.lock`.
     - Verifies current value matches `expected_old` (if provided).
     - Writes `<new_oid>\n`, appends reflog entry to `.git/logs/<ref_name>` (and `.git/logs/HEAD` if updating the current branch!), and commits the lockfile.
   - `delete_ref(&self, ref_name: &str, expected_old: Option<ObjectId>) -> Result<(), GirError>`: removes from both loose ref and `packed-refs` under lock.

### Task 3.2: Revision Expression Parser (`revision/mod.rs`)
1. Support full Git revision syntax:
   - `@` or `HEAD`, branch names, tag names, remote refs (`origin/main`), full 40-char hex SHAs, and unambiguous 4..39-char short SHA prefixes.
   - `~N` (first-parent ancestor $N$ steps back; `~` defaults to `~1`).
   - `^N` ($N$-th parent of a merge commit; `^` defaults to `^1`, `^0` peels tag to commit).
   - `@{N}` ($N$-th prior value in `.git/logs/<ref>`).
   - `^{tree}`, `^{commit}`, `^{blob}` (peel object to target type).
   - Ranges: `A..B` and `A...B`.

### Task 3.3: Safety Snapshot & `gir undo` (`reflog/snapshot.rs`)
1. Implement `SafetySnapshotEngine::capture(&self, command_desc: &str) -> Result<Option<ObjectId>, GirError>`.
2. Implement `SafetySnapshotEngine::undo_last(&self) -> Result<UndoReport, GirError>`:
   - Reads the top entry from `.git/logs/refs/gir/snapshots/history` (or `.git/logs/HEAD` if only a commit/ref move occurred).
   - Restores `HEAD`, the target branch reference, `.git/index`, and working tree files to the exact snapshot state.
   - Records the undo action itself in the reflog so even an undo can be inspected.

### Task 3.4: CLI Commands (`gir-cli`)
1. `gir update-ref [-d] <refname> <newvalue> [<oldvalue>]`
2. `gir symbolic-ref <name> [<ref>]`
3. `gir rev-parse [--short] [--verify] [--abbrev-ref] <rev>...`
4. `gir reflog [show | expire] [<ref>]`
5. `gir undo [--list | <snapshot-id>]`

---

## 5. Differential Verification Suite

1. **`rev-parse` Torture Test**: Build a repository with branches, annotated tags, merge commits (2 parents), and packed refs. Assert `gir rev-parse HEAD~2^2`, `gir rev-parse v1.0^{tree}`, `gir rev-parse HEAD@{1}`, and short 7-char SHAs return the exact same output as `/usr/bin/git rev-parse`.
2. **Reflog Compatibility Test**: Perform ref updates via `gir update-ref`, then run `/usr/bin/git reflog show` and assert `/usr/bin/git` parses every timestamp, author, and message cleanly.
3. **`gir undo` Zero-Data-Loss Test**: Modify tracked files, create new untracked files, record a pre-op snapshot, wipe the working directory, run `gir undo`, and assert every file and byte is restored and `git fsck --full` passes.
