# `gir` (Git in Rust) — Master Implementation Plan Index

**Design Specification**: [`../2026-10-25-gir-design.md`](../2026-10-25-gir-design.md)  
**Repository**: `github.com/prostiate/git-in-rust`  
**Target Binary**: `gir` (1:1 Git replacement + `gir undo` safety engine + `gir tui`)  

---

## 1. How This Plan is Structured

Because `gir` is a full, zero-shortcut systems engineering implementation, the implementation plan is divided into **8 focused milestone documents**. Each document contains:
1. **Systems & Rust Concepts Mastered**: What you learn as an engineer in that milestone.
2. **Exact File & Module Layout**: Every Rust file and public type created.
3. **Byte-Level Binary & Algorithmic Specifications**: Exact packet headers, bitmasks, and invariants so nothing is guessed.
4. **Step-by-Step Implementation Tasks**: Sequenced TDD tasks with concrete Rust structs and method signatures.
5. **1:1 Differential Verification Suite**: Automated tests comparing `gir` against `/usr/bin/git` (`git fsck --full`, byte-for-byte SHA/index checks).

---

## 2. Milestone Plan Index

| # | Milestone Plan Document | Core Subsystem | Commands Delivered | Rust Concepts Taught |
| :--- | :--- | :--- | :--- | :--- |
| **01** | [`01-milestone-1-foundation-and-loose-objects.md`](./01-milestone-1-foundation-and-loose-objects.md) | Workspace, Atomic `LockFile` + `fsync`, `.git/config`, Loose Objects (`Blob`, `Tree`, `Commit`, `Tag`) | `init`, `config`, `hash-object`, `cat-file`, `ls-tree` | Workspace crates, custom `Error` enums, newtype pattern (`ObjectId([u8; 20])`), `Read`/`Write` streaming traits, RAII `Drop` guards, zero-copy `&[u8]` parsing |
| **02** | [`02-milestone-2-packfile-engine.md`](./02-milestone-2-packfile-engine.md) | Unified ODB, `.idx` v2 `mmap` Lookup, `.pack` Delta Decoder (`OFS_DELTA`, `REF_DELTA`), Sliding-Window Pack Writer & GC | `index-pack`, `unpack-objects`, `pack-objects`, `fsck`, `gc` | `unsafe` boundary encapsulation with `memmap2`, variable-length integer bit manipulation, recursive delta reconstruction, `Mutex`/`RwLock` LRU caching |
| **03** | [`03-milestone-3-refs-reflog-and-safety-snapshots.md`](./03-milestone-3-refs-reflog-and-safety-snapshots.md) | Loose Refs, `packed-refs`, Revision Parser (`rev-parse`), `.git/logs/` Reflog, Pre-Operation Safety Snapshots | `update-ref`, `symbolic-ref`, `rev-parse`, `reflog`, `undo` | Recursive descent / combinator parsing for revisions (`HEAD~3^2@{1}`), atomic transaction guards, crash-safe append logs |
| **04** | [`04-milestone-4-ignore-index-and-status.md`](./04-milestone-4-ignore-index-and-status.md) | Hierarchical `.gitignore`, Binary `.git/index` (DIRC v2–v4 + `TREE`/`REUC` extensions), Parallel Stat Scanner & Upstream Tracking | `status`, `add`, `rm`, `mv`, `ls-files` | Big-endian binary struct packing, POSIX `MetadataExt` (`ino`, `mtime`, `ctime`, `mode`), `rayon` data-parallel directory traversal |
| **05** | [`05-milestone-5-trees-commits-branches-and-log.md`](./05-milestone-5-trees-commits-branches-and-log.md) | Index-to-Tree Builder, Commit Authoring + Git Hooks, Branch (`-m`/`-M` unborn & committed) & Tag Manager, Priority-Queue DAG Walker | `write-tree`, `commit-tree`, `commit`, `branch`, `tag`, `log`, `show` | `BinaryHeap` priority queues, topological graph sorting (Kahn's / in-degree), custom `Iterator` implementation for lazy DAG streaming |
| **06** | [`06-milestone-6-diff-checkout-reset-and-stash.md`](./06-milestone-6-diff-checkout-reset-and-stash.md) | Myers $O(ND)$ Diff, Interactive Hunk Staging (`add -p`), Safe Worktree Transitions, Multi-Parent Stash | `diff`, `switch`, `checkout`, `reset`, `restore`, `clean`, `stash` | Dynamic programming / Myers shortest edit script, lifetime-bound slice diffing, transactional filesystem state machines |
| **07** | [`07-milestone-7-merge-cherry-pick-and-rebase.md`](./07-milestone-7-merge-cherry-pick-and-rebase.md) | Lowest Common Ancestor (LCA), 3-Way Recursive Merge, Index Stages 1/2/3, `.git/sequencer` Engine | `merge-base`, `merge`, `cherry-pick`, `rebase` (`-i`, `--continue`, `--abort`) | Graph bidirectional BFS for merge-base, 3-way line-chunk state machines, persistent state machine serialization |
| **08** | [`08-milestone-8-wire-transport-and-tui.md`](./08-milestone-8-wire-transport-and-tui.md) | `pkt-line` Wire Protocol, Credential Helper & SSH Agent, Smart HTTP(S) & SSH (`upload-pack` / `receive-pack`), `ratatui` Interactive TUI | `remote`, `clone`, `fetch`, `pull`, `push`, `tui` | Network protocol state machines, multiplexed sideband stream demuxing, immediate-mode terminal rendering (`ratatui`) |

---

## 3. Cargo Workspace Architecture

```text
git-in-rust/
├── Cargo.toml                  # [workspace] members = ["crates/*"]
├── docs/
│   ├── 2026-10-25-gir-design.md
│   └── plan/
│       ├── 00-index.md
│       ├── 01-milestone-1-foundation-and-loose-objects.md
│       ├── 02-milestone-2-packfile-engine.md
│       ├── 03-milestone-3-refs-reflog-and-safety-snapshots.md
│       ├── 04-milestone-4-ignore-index-and-status.md
│       ├── 05-milestone-5-trees-commits-branches-and-log.md
│       ├── 06-milestone-6-diff-checkout-reset-and-stash.md
│       ├── 07-milestone-7-merge-cherry-pick-and-rebase.md
│       └── 08-milestone-8-wire-transport-and-tui.md
└── crates/
    ├── gir-core/               # Core storage, index, refs, diff, merge, snapshots
    ├── gir-transport/          # pkt-line, Smart HTTP/SSH client & server primitives (shared with GirHub)
    ├── gir-tui/                # Ratatui interactive terminal interface
    └── gir-cli/                # Binary `gir` CLI
```

---

## 4. The Golden Verification Rule (Differential Testing)

Every milestone must pass a **Differential Test Suite** (`crates/gir-core/tests/differential_*.rs` and `crates/gir-cli/tests/`):
1. **Write with `gir`, Verify with `git`**:
   - Every repository mutated by `gir` must pass `git fsck --full --strict` with zero warnings or errors.
   - Hashes generated by `gir hash-object`, `gir write-tree`, and `gir commit-tree` must match `git` byte-for-byte.
2. **Write with `git`, Read with `gir`**:
   - Repositories initialized, committed, packed (`git gc --aggressive`), merged, and stashed by official `git` must produce identical outputs when inspected with `gir status`, `gir log`, `gir diff`, and `gir cat-file`.
3. **Crash & Mistake Recovery Verification**:
   - Every destructive command (`reset --hard`, `checkout`, `clean`, `merge`) is tested to verify that `gir undo` restores 100% of uncommitted working tree and staged modifications.
