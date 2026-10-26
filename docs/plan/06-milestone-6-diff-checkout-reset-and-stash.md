# Milestone 6: Myers Diff, Interactive Hunk Staging, Checkout, Reset, Restore & Stash

**Parent Index**: [`00-index.md`](./00-index.md)  
**Target Crates**: `gir-core`, `gir-cli`  
**Commands Delivered**: `gir diff`, `gir add -p`, `gir switch`, `gir checkout`, `gir reset`, `gir restore`, `gir clean`, `gir stash`  

---

## 1. What You Will Learn in This Milestone

### Systems Engineering Concepts
- **Myers $O(ND)$ Shortest Edit Script (SES) Algorithm**: How Eugene Myers' graph search finds the minimum number of line insertions and deletions between two files by searching diagonals $k = x - y$ for the furthest-reaching $D$-path.
- **Unified Diff Hunk Windowing & Patch Application**: How raw edit operations (`Keep`, `Insert`, `Delete`) are grouped into 3-line context hunks (`@@ -old_start,old_count +new_start,new_count @@`), and how `git add -p` applies a single selected hunk to an in-memory blob and updates `.git/index` without touching the working tree.
- **Safe Working-Tree State Machine (`checkout` / `switch` / `reset`)**: How Git transitions between `(Current Tree H, Index I, Working Tree W)` and a `Target Tree T`, detecting when an untracked or modified working-tree file would be clobbered and aborting **before** a single file on disk is touched.
- **Git's Multi-Parent Stash DAG (`refs/stash`)**: Why a Git stash is not a special file format at all—it is a standard `Commit` object with **2 or 3 parents** (`Parent 1` = `HEAD` commit, `Parent 2` = commit of current `.git/index`, `Parent 3` = optional commit of untracked files)!

### Rust Concepts
- **Zero-Copy Line Tokenization**: Hashing lines into `u64` / `usize` equivalence classes so Myers' inner loop operates on integer arrays instead of string comparisons.
- **Transactional Rollback & Pre-Op Snapshot Guards**: Wrapping `checkout`, `reset --hard`, `restore`, and `clean` in `SafetySnapshotEngine::capture()` so every destructive action is 100% reversible via `gir undo`.

---

## 2. Files to Create / Modify

```text
crates/gir-core/src/
├── diff/
│   ├── mod.rs              # diff_trees, diff_index_to_worktree, diff_tree_to_index
│   ├── myers.rs            # Myers O(ND) shortest edit script over line slices
│   ├── hunk.rs             # DiffHunk, UnifiedPatch, binary file detection (NUL byte scan)
│   └── patch.rs            # apply_hunks_to_blob(base_bytes, selected_hunks) for `add -p` & TUI
├── worktree/
│   ├── mod.rs              # checkout_tree(repo, target_tree, mode: Safe | Force)
│   ├── reset.rs            # reset(repo, target_commit, Soft | Mixed | Hard)
│   └── restore.rs          # restore(repo, paths, staged: bool, worktree: bool, source)
└── stash/
    └── mod.rs              # stash_push, stash_pop, stash_apply, stash_list, stash_drop
crates/gir-cli/src/commands/
├── diff.rs
├── switch.rs
├── checkout.rs
├── reset.rs
├── restore.rs
├── clean.rs
└── stash.rs
```

---

## 3. Algorithmic Specifications

### 3.1 Myers $O(ND)$ Diff & Unified Hunk Grouping (`diff/myers.rs` & `diff/hunk.rs`)
1. **Binary Check**: Scan first 8,000 bytes of old and new blobs. If either contains `0x00` (`\0`), emit `Binary files a/<path> and b/<path> differ` and skip line diffing.
2. **Line Interning**: Split `old` and `new` on `\n` into `Vec<&[u8]>`. Map each unique line slice to a `u32` ID so equality checks during the $O(ND)$ forward/backtracking passes are single `u32 == u32` comparisons.
3. ** Myers Trace**:
   - Maintain array `v: Vec<isize>` of size $2 \cdot (N + M) + 1$ storing furthest $x$ for diagonal $k \in [-D, +D]$.
   - Record snapshot of `v` at each edit distance $d = 0..=D$, then backtrack from $(N, M)$ to $(0, 0)$ to emit `EditOp::Equal`, `EditOp::Delete`, `EditOp::Insert`.
4. **Hunk Windowing (`context = 3` lines)**:
   - Merge any two change runs separated by $\le 2 \times \text{context}$ (`6`) equal lines into a single `DiffHunk` with header `@@ -old_line,old_len +new_line,new_len @@`.

### 3.2 Two-Tree Safe Checkout Matrix (`worktree/mod.rs`)
When switching from `HEAD` tree $H$ to target tree $T$ with current index $I$ and working tree $W$:

| Path in $H$ | Path in $T$ | Index $I$ vs $H$ | Worktree $W$ vs $I$ | Action |
| :--- | :--- | :--- | :--- | :--- |
| Same (`H == T`) | Same (`H == T`) | Dirty or Clean | Dirty or Clean | **Keep untouched** (local changes carry over to target branch!) |
| Absent | Present (`T`) | Absent | Untracked file exists | **ABORT** (`untracked working tree file would be overwritten`) |
| Present (`H`) | Modified (`T != H`) | Clean (`I == H`) | Clean (`W == I`) | Update file on disk to `T` and update `I = T` |
| Present (`H`) | Modified (`T != H`) | Dirty or Clean | Dirty (`W != I` or `I != H`) | **ABORT** (`your local changes would be overwritten by checkout`) |
| Present (`H`) | Deleted (`Absent`) | Clean (`I == H`) | Clean (`W == I`) | Delete file from disk (and empty parent dirs) and remove from `I` |

**Pre-Validation Guarantee**: Phase 1 checks **all** paths against the table above. Only if **zero** conflicts exist does Phase 2 capture a safety snapshot and perform the disk/index writes!

### 3.3 Multi-Parent Stash Layout (`stash/mod.rs`)
```text
       +--- (P3: optional untracked files commit `U`)
      /
(Stash Commit `W`: working tree state)
   |  \
   |   +--- (P2: staged index commit `I`, parent = `HEAD`)
   |
(P1: base `HEAD` commit `B`)
```
- `stash push`:
  1. Creates commit `I` from current `.git/index` with parent `B` (`HEAD`).
  2. Creates commit `W` from working tree tracked modifications with parents `[B, I]` (and `U` if `-u` / `--include-untracked`).
  3. Updates `.git/refs/stash` (and appends to `.git/logs/refs/stash` with `"WIP on <branch>: <short-sha> <subject>"`).
  4. Resets working tree and `.git/index` cleanly to `HEAD`.
- `stash apply` / `stash pop`:
  1. Applies the changes from `B -> W` onto the working tree and `B -> I` onto the index (if `--index`).
  2. If `pop` and no conflicts occurred, drops the top entry from `.git/logs/refs/stash`.

---

## 4. Step-by-Step Implementation Tasks

### Task 6.1: Myers Diff, Patch Formatter & `gir diff`
1. Implement `myers_diff`, `UnifiedPatch`, and tree/index/worktree comparators.
2. Implement `gir diff [--staged | --cached] [--stat] [--name-only] [<rev1> [<rev2>]] [-- <path>...]`.

### Task 6.2: Hunk Applier & `gir add -p`
1. Implement `apply_hunk_to_lines(index_lines, hunk, reverse: bool)`.
2. Implement interactive `gir add -p` loop (`y` stage hunk, `n` skip, `q` quit, `a` stage all remaining in file, `d` skip file).

### Task 6.3: Safe Checkout, Switch, Reset, Restore & Clean
1. Integrate `SafetySnapshotEngine::capture` at the entry of `checkout`, `reset --hard`, `restore`, and `clean`.
2. Implement `gir switch [-c <new-branch>] <branch>`.
3. Implement `gir checkout [-b <new-branch>] [-f] <branch-or-commit> [-- <paths>...]`.
4. Implement `gir reset [--soft | --mixed | --hard] [<rev>] [-- <paths>...]`.
5. Implement `gir restore [-s <source>] [-S | --staged] [-W | --worktree] <paths>...`.
6. Implement `gir clean [-n | --dry-run] [-f] [-d] [-x]`.

### Task 6.4: Stash Engine
1. Implement `gir stash [push [-m <msg>] [-u]] | pop | apply [<stash>] | list | show [-p] | drop [<stash>] | clear`.

---

## 5. Differential Verification Suite

1. **`gir diff` Unified Hunk Compatibility**:
   - Create multi-hunk edits across files, run `gir diff > gir.patch`, and apply `gir.patch` on a clean clone using `/usr/bin/git apply --check gir.patch`. Assert `/usr/bin/git apply` accepts every hunk without error.
2. **Dirty Checkout Protection & Carry-Over Test**:
   - Modify file `a.txt` (which is identical between `main` and `feature`) and `b.txt` (which differs between `main` and `feature`).
   - Verify `gir switch feature` aborts cleanly citing `b.txt`, leaving both files untouched.
   - Revert `b.txt`, run `gir switch feature`, and verify `a.txt`'s uncommitted changes are carried over intact.
3. **Cross-Tool Stash Test**:
   - Run `gir stash push -m "work in progress"`, then run `/usr/bin/git stash list` and `/usr/bin/git stash pop`. Assert official `git` pops the stash created by `gir` cleanly (and vice versa).
