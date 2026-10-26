# Design Document: `gir` (Git in Rust — Complete 1:1 Engine, TUI & Transport)

**Date**: 2026-10-26  
**Author**: Muhammad Irfan Kurniawan (`@prostiate`)  
**Repository**: `github.com/prostiate/git-in-rust`  
**Status**: Approved Production Systems Specification  

---

## 1. Intent and The Why

This project is Milestone 1 of Irfan's 4-part Core Systems Engineering curriculum (`gir` + `GirHub` $\rightarrow$ `http server` $\rightarrow$ `database` $\rightarrow$ `bun`), designed to bridge the gap from Senior Full-Stack/Infrastructure Engineering to **Core Systems Engineering**.

Having already built production distributed systems and `OnoGitTree` (a desktop multi-repository manager in Go + Wails that shelled out to the host `git` CLI), the goal of `gir` is to eliminate the `git` black box completely:
- **Master Rust Systems Programming**: Ownership, borrowing, lifetimes, zero-copy byte slicing (`&[u8]`), memory-mapped I/O (`mmap`), streaming compression, bit-level binary protocol parsing, and lock-free/parallel execution.
- **Own Every Layer of Version Control**: Content-addressable storage, sliding-window zlib compression, `.idx`/`.pack` binary formats and delta chains, `.git/index` stat caching, `.gitignore` globbing, Myers diffing, 3-way merge algorithms, and Git's Smart HTTP/SSH wire transfer protocols (`pkt-line`, `upload-pack`, `receive-pack`).
- **Build a True Daily-Driver Tool**: `gir` must be a 1:1 drop-in replacement for `git` on real-world repositories (Nuxt, Go, Rust, monorepos) with an embedded interactive TUI (`gir tui`) and **stronger data-loss guarantees than standard Git**.
- **Power `GirHub` Directly**: The storage and wire-protocol crates built here (`gir-core` and `gir-transport`) will be imported directly by `GirHub` to serve repositories over HTTP/SSH without shelling out.

---

## 2. Goals, Non-Goals, and Anti-Goals

### Goals
1. **Native `.git` Interoperability (Zero Ecosystem Friction)**:
   - Operates strictly and natively on the standard `.git` directory—no separate `.gir` directory gimmick—so VS Code, Cursor, terminal prompts, `Turborepo`, `OnoGitTree`, and official `git` work alongside `gir` simultaneously without migration.
   - Any repository created, staged, committed, merged, or packed by `gir` passes `git fsck --full` and works seamlessly with official `git` at the exact same time.
   - Any existing cloned repository on disk works immediately with `gir`.
2. **Dual-Layer Zero Data Loss Architecture**:
   - **Layer 1 — Hardware & Crash Durability**: Every write to `.git/objects`, `.git/index`, `.git/refs`, and `.git/packed-refs` uses atomic write-to-temp, explicit `fsync` (`File::sync_all()`), and atomic POSIX rename. Concurrent access is guarded via `O_CREAT | O_EXCL` `.lock` files.
   - **Layer 2 — Human & Operation Safety (Reflog + Pre-Operation Snapshots)**:
     - Full `.git/logs/` **Reflog Engine** tracking every `HEAD` and branch movement (`commit`, `checkout`, `reset`, `merge`, `rebase`, `pull`) in standard Git reflog format.
     - **Automatic Pre-Operation Safety Snapshots**: Before *any* potentially destructive working-tree or history mutation (`reset`, `checkout`, `restore`, `clean`, `merge`, `rebase`, `stash drop`), `gir` automatically snapshots dirty tracked and modified state into standard loose blob/tree/commit objects referenced under `refs/gir/snapshots/<timestamp>-<op>`.
     - **`gir undo`**: One-command instant restoration of the previous working-tree, index, and `HEAD` state from the latest snapshot or reflog entry—making accidental data loss from bad merges or `reset --hard` impossible.
3. **First-Class Unified Storage Engine (Loose + Packfile Read/Write)**:
   - Transparently reads and writes both loose objects (`.git/objects/xx/yyyy...`) and packed archives (`.git/objects/pack/*.idx` v2 and `*.pack`), including full `OBJ_OFS_DELTA` and `OBJ_REF_DELTA` reconstruction and delta-compressed packfile generation.
4. **Complete Real-World Repository Mechanics**:
   - **`.gitignore` Engine**: Evaluates `.git/info/exclude`, global `~/.config/git/ignore`, and hierarchical `.gitignore` files across nested directories (`*`, `?`, `[...]`, `**`, directory-only `/`, and `!` negation rules).
   - **Config & Upstream Tracking Engine**: Parses and writes `.git/config` and `~/.gitconfig` (`user.name`, `user.email`, `[core]`, `[remote "<name>"]`, `[branch "<name>"]` upstream tracking for zero-arg `gir pull`/`gir push` and ahead/behind counts in `gir status`).
   - **Git Hooks Engine**: Executes `.git/hooks/*` and custom `core.hooksPath` (e.g., `.husky/` for `pre-commit`, `commit-msg`, `post-commit`, `pre-push`), with `-n` / `--no-verify` bypass support.
   - **Credential Helper & SSH Agent Integration**: Speaks the standard Git Credential Helper protocol (`credential.helper` in `~/.gitconfig`, e.g., `gh auth git-credential`, `store`, `cache`) and `SSH_AUTH_SOCK` so pushing/pulling to private and public GitHub repos works out of the box.
   - **Unified Reference Store**: Resolves symbolic refs (`HEAD`), loose refs (`.git/refs/{heads,tags,remotes}/*`), and packed refs (`.git/packed-refs` with peeled tag support).
   - **Full `.git/index` (DIRC v2/v3/v4) with Extensions**: Parses and writes the binary staging index with stat-cache fast paths, conflict stages (`0..=3`), and proper extension handling—specifically invalidating/updating the `TREE` (cache-tree) extension and supporting `REUC` (resolve-undo) so official `git` and `gir` never desynchronize.
5. **Complete 1:1 Command Suite**:
   - **Plumbing**: `init`, `hash-object`, `cat-file`, `ls-files`, `ls-tree`, `write-tree`, `commit-tree`, `update-ref`, `rev-parse`, `symbolic-ref`, `merge-base`, `pack-objects`, `index-pack`, `unpack-objects`, `fsck`.
   - **Porcelain (Local Workflow)**: `status` (including upstream ahead/behind counts), `add` (including `add -p` interactive hunk staging), `rm`, `mv`, `commit` (with hooks & `--amend`), `log` (with `--graph`, `--oneline`, `--stat`), `show`, `diff` (worktree, `--staged`, commit-to-commit), `branch` (including `-m`/`-M` on unborn & committed branches), `switch`, `checkout`, `reset` (`--soft`, `--mixed`, `--hard`), `restore`, `clean`, `tag` (lightweight & annotated), `stash` (`push`, `pop`, `apply`, `list`, `drop`), `reflog`, `undo`, `gc`.
   - **Porcelain (History & Branch Integration)**: `merge` (fast-forward and 3-way recursive merge with conflict markers), `cherry-pick`, `rebase` (linear and interactive `-i` todo execution, `--continue`, `--abort`).
   - **Porcelain (Remote Network Sync)**: `remote` (`add`, `remove`, `set-url`, `-v`), `clone`, `fetch`, `pull`, `push` (including `-u` / `--set-upstream` and `--force-with-lease`) over Smart HTTP(S) and SSH.
6. **Built-In High-Speed TUI (`gir tui`)**:
   - Integrated terminal UI built with `ratatui` + `crossterm` calling `gir-core` directly in-memory (0ms CLI spawn overhead):
     - File tree & status pane with 1-key stage/unstage.
     - Interactive diff & hunk/line staging viewer.
     - Commit DAG graph browser and commit inspector.
     - Branch, remote, and stash controller.
     - Safety Snapshot & Reflog time-machine panel for 1-key `undo` recovery.

### Non-Goals
- **Legacy 1990s Git Bloat**: Obscure commands and flags that teach no new systems-engineering principles (`git send-email`, `git whatchanged`, `git rerere`, `git notes`, `git archive`, `git cvsserver`, `git svn`, Tcl/Tk `gitk`/`git-gui`) are explicitly excluded so momentum stays focused on the 4-project systems roadmap (`gir` $\rightarrow$ `http server` $\rightarrow$ `database` $\rightarrow$ `bun`).
- **Separate `.gir` Directory**: `gir` never renames `.git/` to `.gir/` so IDE integrations, shell prompts, and monorepo tools never break.
- **Experimental SHA-256 Object Format in v1**: Standard 20-byte SHA-1 format is targeted for 100% ecosystem compatibility, though internal `ObjectId` structs wrap `[u8; 20]` cleanly to allow future extension.

### Anti-Goals
- **Zero Subprocess Shelling**: `gir` must never invoke `/usr/bin/git`. Every algorithm and protocol is native Rust.
- **Zero Toy Shortcuts**: No skipping `.gitignore`, no ignoring index extensions, no fake in-memory stores.
- **Zero Unrecoverable Destructive Actions**: Never overwrite untracked files or dirty working tree files without either failing safely or recording a content-addressed snapshot in `refs/gir/snapshots/`.

---

## 3. Constraints & Dependencies

- **Language & Toolchain**: Stable Rust (Edition 2021 / 2024).
- **Primary OS**: Linux (`x86_64`, POSIX filesystem semantics, permissions `0644` / `0755` / `120000` symlinks).
- **Curated Dependencies**:
  - `flate2`: Streaming zlib deflate/inflate.
  - `sha1` & `crc32fast`: Cryptographic SHA-1 hashing and CRC32 verification for `.idx` v2.
  - `memmap2`: Safe memory-mapped file I/O for `.pack`, `.idx`, and `.git/index`.
  - `clap`: CLI argument and subcommand parsing compatible with Git flags.
  - `ratatui` & `crossterm`: Terminal UI rendering and keyboard/mouse event handling.
  - `reqwest` (with `rustls`) & `ssh2` (or native SSH subprocess pipe): Smart HTTP(S) and SSH transport channels.
  - `rayon`: Data-parallel working tree stat scanning, blob hashing, and delta compression.
  - `tempfile`: Atomic temporary file creation on the target mount point.
  - `thiserror`: Structured error types across crates.

---

## 4. Approaches Considered

### Approach 1: Multi-Crate Layered Workspace (`gir-core` + `gir-transport` + `gir-tui` + `gir-cli`) [CHOSEN]
- **Description**: A single Cargo workspace housing four cleanly separated crates:
  1. `gir-core`: Pure repository engine (objects, packfiles, index, refs, reflog, snapshots, config, ignore, diff, merge, rebase).
  2. `gir-transport`: Wire protocol engine (`pkt-line` parser/encoder, Smart HTTP/SSH client & server primitives, pack negotiation).
  3. `gir-tui`: `ratatui` interactive terminal application powered directly by `gir-core` and `gir-transport`.
  4. `gir-cli`: The `gir` binary exposing all plumbing/porcelain CLI commands and `gir tui`.
- **Trade-offs**: Requires disciplined crate interfaces up front, but keeps terminal UI, network I/O, and disk algorithms decoupled.
- **Why Chosen**:
  - `gir-cli` and `gir-tui` share 100% of the same atomic storage and safety snapshot guarantees.
  - When we build **`GirHub`** next, the `GirHub` backend imports `gir-core` and `gir-transport` directly to handle `git-upload-pack` and `git-receive-pack` requests in pure Rust.

### Approach 2: Monolithic Single-Crate Binary [REJECTED]
- **Why Rejected**: Couples CLI stdout formatting and TUI event loops with storage primitives, making `GirHub` reuse impossible without extracting crates later.

### Approach 3: Loose-Object-Only Prototype First [REJECTED]
- **Why Rejected**: Fails immediately on real cloned repositories where history is stored in `.pack` files and `.git/packed-refs`, violating the requirement that `gir` be usable on real projects.

---

## 5. Architectural Design

```
git-in-rust/
├── Cargo.toml                        # Workspace root
├── docs/
│   └── 2026-10-25-gir-design.md      # This design specification
└── crates/
    ├── gir-core/                     # Storage, Index, Graph, Diff & Merge Engine
    │   ├── Cargo.toml
    │   └── src/
    │       ├── lib.rs
    │       ├── error.rs              # Strongly-typed GirError
    │       ├── hash.rs               # 20-byte ObjectId (SHA-1) & hex conversions
    │       ├── config/               # .git/config & ~/.gitconfig INI parser/writer
    │       ├── ignore/               # Hierarchical .gitignore & exclude matcher
    │       ├── storage/              # Atomic Lockfile, Tempfile+fsync, Unified ODB
    │       ├── object/               # Blob, Tree, Commit, Tag parser & serializer
    │       ├── pack/                 # .idx v2 mmap lookup, .pack reader/writer, delta codec
    │       ├── refs/                 # Loose refs, packed-refs, symbolic refs, HEAD
    │       ├── reflog/               # .git/logs/* reader/appender + Pre-Op Snapshots
    │       ├── index/                # DIRC v2/v3/v4 parser/writer, stat cache, extensions
    │       ├── status/               # Parallel HEAD-vs-Index-vs-Worktree scanner
    │       ├── diff/                 # Myers diff, patience diff, patch/hunk application
    │       ├── revision/             # rev-parse syntax (HEAD~3, branch@{1}, ranges)
    │       ├── merge/                # LCA finder, 3-way tree/file merge, conflict markers
    │       ├── rebase/               # Sequencer for cherry-pick, rebase & interactive todo
    │       ├── stash/                # 2-parent / 3-parent stash commit construction
    │       └── repository.rs         # High-level transactional Repository API
    ├── gir-transport/                # Git Wire Protocol & Pack Negotiation
    │   ├── Cargo.toml
    │   └── src/
    │       ├── lib.rs
    │       ├── pktline.rs            # 4-byte hex length-prefixed pkt-line framing
    │       ├── protocol.rs           # Reference discovery, want/have negotiation, sideband
    │       ├── http.rs               # Smart HTTP client (info/refs, upload-pack, receive-pack)
    │       └── ssh.rs                # SSH transport channel
    ├── gir-tui/                      # Interactive Terminal UI (ratatui)
    │   ├── Cargo.toml
    │   └── src/
    │       ├── lib.rs
    │       ├── app.rs                # Event loop, keybindings, state machine
    │       └── views/                # Status, Diff/Hunk stage, Graph Log, Stash, Undo/Reflog
    └── gir-cli/                      # Binary entrypoint (`gir`)
        ├── Cargo.toml
        └── src/
            ├── main.rs
            └── commands/             # 1:1 handlers for all plumbing & porcelain commands
```

### 5.1 Atomic Durability & Lockfile Protocol
1. **Lockfile Acquisition (`LockFile`)**:
   - Opens `<target>.lock` using `OpenOptions::new().write(true).create_new(true)` (`O_CREAT | O_EXCL`).
   - Retries with a 100ms quadratic backoff (`1ms, 4ms, 9ms, 16ms, 25ms, 50ms`, matching Git's `core.filesRefLockTimeout`) so concurrent background `git status` commands from VS Code, Cursor, or `OnoGitTree` never cause false lock errors. If still locked after timeout, aborts safely without corrupting state.
   - Implements `Drop` to automatically clean up uncommitted `.lock` files on panic or early error return.
2. **Commit to Disk**:
   - Writes buffered payload $\rightarrow$ calls `file.sync_all()` (`fsync`) $\rightarrow$ performs `std::fs::rename(&lock_path, &target_path)` $\rightarrow$ `fsync`s the parent directory descriptor to guarantee directory entry durability on Linux ext4/xfs/btrfs.

### 5.2 Pre-Operation Safety Snapshot & `gir undo`
Before executing any destructive or working-tree-mutating command (`reset`, `checkout`, `restore`, `clean`, `merge`, `rebase`):
1. `gir-core` checks if the working tree or index differs from `HEAD`.
2. If dirty, `gir-core` writes any modified/untracked working tree files as standard loose blobs, constructs a snapshot tree + commit object (storing both index tree and working-tree state, identical in structure to a Git stash commit), and records it at `.git/refs/gir/snapshots/<unix-ms>-<command>` plus an entry in `.git/logs/refs/gir/snapshots`.
3. Running `gir undo` inspects the most recent operation in the reflog/snapshot log and atomically restores `HEAD`, branch refs, `.git/index`, and working tree files to their exact pre-command state.

### 5.3 Unified Object Database (`Loose` + `Packfile` Read/Write)
- **Read Path**:
  1. Check in-memory LRU cache of recently decoded objects.
  2. Check `.git/objects/<first-2-hex>/<remaining-38-hex>` (zlib inflate + header parse).
  3. Search memory-mapped `.git/objects/pack/*.idx` (v2):
     - Read 256-entry 4-byte big-endian fanout table to bound the binary search range $[lo, hi)$.
     - Binary search the 20-byte SHA table; read the corresponding 4-byte (or 8-byte for $>2\text{ GB}$ packs) offset.
     - Decode the entry in `.pack`: if `OBJ_COMMIT`/`OBJ_TREE`/`OBJ_BLOB`/`OBJ_TAG` (types 1–4), inflate directly. If `OBJ_OFS_DELTA` (type 6) or `OBJ_REF_DELTA` (type 7), recursively resolve base object and apply the copy/insert delta instruction stream.
- **Write Path**:
  - Local commands write loose objects atomically.
  - `gir pack-objects` / `gir push` / `gir gc` sorts reachable objects by type and path basename hash, computes sliding-window deltas, writes `.pack` stream with trailing SHA-1 checksum, and builds the corresponding `.idx` v2 file with CRC32 checksums.

### 5.4 `.git/index` Engine & Fast Parallel `status`
- Parses and writes DIRC v2/v3/v4 headers, sorted index entries, and trailing extensions.
- Whenever index entries are added/modified/removed, any stale `TREE` extension path is invalidated so official `git` never builds a commit from stale cache-trees.
- `gir status` uses `rayon` to `lstat` working tree paths in parallel (pruning ignored directories immediately via the `.gitignore` engine) and compares `(mtime, ctime, size, ino, mode)` against the index stat cache. Only files with changed stat metadata are read and hashed—matching `git status` sub-10ms speed on large repos.

---

## 6. Engineering Implementation Milestones

### Milestone 1: Workspace Foundation, Atomic Storage & Loose Object Plumbing
- Initialize Cargo workspace (`gir-core`, `gir-transport`, `gir-tui`, `gir-cli`).
- Implement `ObjectId`, atomic `LockFile` (with 100ms backoff) / `TempFile` with `fsync` + parent dir sync.
- Implement `Blob`, `Tree`, `Commit`, `Tag` serialization and streaming zlib/SHA-1.
- Implement `.git/config` INI parser/writer.
- Commands: `gir init`, `gir config`, `gir hash-object`, `gir cat-file`, `gir ls-tree`.
- Cross-verification test harness asserting 1:1 byte identity with `git`.

### Milestone 2: Unified Packfile Engine (`.idx` v2 + `.pack` Reader, Delta Resolver, Writer & GC)
- Implement memory-mapped `.idx` v2 parser with CRC32 and 64-bit offset support.
- Implement `.pack` object reader and `OBJ_OFS_DELTA` / `OBJ_REF_DELTA` delta instruction decoder.
- Implement `pack-objects` generator with sliding-window delta compression, `index-pack`, and `gc`.
- Commands: `gir index-pack`, `gir unpack-objects`, `gir pack-objects`, `gir fsck`, `gir gc`.
- Verify `gir cat-file -p` and `gir fsck` on real multi-thousand-commit packed repositories.

### Milestone 3: Reference Manager, Reflog, Revision Parser & Safety Snapshots (`gir undo`)
- Implement loose refs, `.git/packed-refs` parser/writer, and symbolic ref resolution (`HEAD`).
- Implement revision parser (`HEAD~n`, `HEAD^n`, `<branch>@{n}`, short SHA prefix disambiguation, `A..B` ranges).
- Implement `.git/logs/` Reflog reader/appender and the Pre-Operation Safety Snapshot engine (`refs/gir/snapshots/*`).
- Commands: `gir update-ref`, `gir symbolic-ref`, `gir rev-parse`, `gir reflog`, `gir undo`.

### Milestone 4: `.gitignore` Engine, Binary `.git/index` (DIRC), Parallel `status` & Upstream Tracking
- Implement hierarchical `.gitignore` / `.git/info/exclude` globbing and negation engine.
- Implement binary `.git/index` v2/v3/v4 parser and writer with `TREE` and `REUC` extension handling.
- Implement parallel working-tree scanner with stat-cache comparison and upstream ahead/behind commit counting.
- Commands: `gir status`, `gir add`, `gir rm`, `gir mv`, `gir ls-files`.

### Milestone 5: Trees, Commits, Git Hooks, Branches (`-m`/`-M`), Tags & Commit Graph Traversal (`log` / `show`)
- Implement index-to-tree builder (`write-tree`) supporting nested subdirectories and executable/symlink modes.
- Implement commit creation (`commit-tree`, `commit -m`, `commit -a`, `--amend`) with config author/committer lookup, reflog recording, and Git hooks execution (`pre-commit`, `commit-msg`, `post-commit`, respecting `core.hooksPath` and `--no-verify`).
- Implement branch management including `-m` / `-M` rename on both unborn and committed branches.
- Implement priority-queue topological/date-ordered commit DAG walker with ASCII graph renderer.
- Commands: `gir write-tree`, `gir commit-tree`, `gir commit`, `gir branch`, `gir tag`, `gir log`, `gir show`.

### Milestone 6: Myers Diff, Interactive Patching, Checkout, Reset, Restore & Stash
- Implement Myers diff algorithm, unified patch formatter, and hunk-level patch applier (`add -p`).
- Implement safe working-tree transition engine (verifying no untracked/dirty overwrites, recording pre-op snapshot, updating worktree + index + `HEAD` atomically).
- Implement multi-parent stash commits (`refs/stash` + `.git/logs/refs/stash`).
- Commands: `gir diff`, `gir switch`, `gir checkout`, `gir reset`, `gir restore`, `gir clean`, `gir stash`.

### Milestone 7: 3-Way Merge, Conflict Resolution, Cherry-Pick & Rebase Sequencer
- Implement Lowest Common Ancestor (LCA / `merge-base`) search over the commit DAG.
- Implement fast-forward merge and 3-way tree/file merge (`<<<<<<<`, `=======`, `>>>>>>>` conflict markers, writing stages 1/2/3 into `.git/index`).
- Implement `.git/sequencer` state machine for `cherry-pick` and `rebase` (`--continue`, `--abort`, `--skip`, and interactive `-i` todo list: `pick`, `reword`, `squash`, `fixup`, `drop`).
- Commands: `gir merge-base`, `gir merge`, `gir cherry-pick`, `gir rebase`.

### Milestone 8: Smart HTTP/SSH Wire Transport, Credential Helper & Interactive TUI (`gir tui`)
- Implement `pkt-line` framing, capability negotiation, `want`/`have` pack negotiation, sideband demultiplexing, Git Credential Helper protocol (`credential.helper`), and `SSH_AUTH_SOCK` in `gir-transport` (plus server-side handlers ready for `GirHub`).
- Commands: `gir remote`, `gir clone`, `gir fetch`, `gir pull`, `gir push` (with `-u` / `--set-upstream` and `pre-push` hook execution).
- Implement `gir-tui` (`gir tui`) with status/staging pane, hunk diff viewer, commit graph viewer, branch/stash manager, and visual safety-snapshot/reflog undo explorer.

---

## 7. What is Left to the Builder

- Internal module visibility and helper traits within `gir-core`.
- Exact LRU cache sizing for decoded packfile base objects (e.g., 64 MB default).
- ANSI color palette defaults and `$PAGER` (`less -FRX`) spawning heuristics in `gir-cli`.
- Integration test suite structure running automated differential tests (`gir` vs `/usr/bin/git`) across every milestone.
