# Milestone 7: 3-Way Merge, Conflict Resolution, Cherry-Pick & Rebase Sequencer

**Parent Index**: [`00-index.md`](./00-index.md)  
**Target Crates**: `gir-core`, `gir-cli`  
**Commands Delivered**: `gir merge-base`, `gir merge`, `gir cherry-pick`, `gir rebase`  

---

## 1. What You Will Learn in This Milestone

### Systems Engineering Concepts
- **Lowest Common Ancestor (LCA / `merge-base`) on a DAG**: How Git finds the best common ancestor commit $B$ between two diverged branches $O$ (`Ours`) and $T$ (`Theirs`) using bidirectional BFS and generation/timestamp pruning (and why a single 2-way diff between $O$ and $T$ can never know whether a line was added by $O$ or deleted by $T$!).
- **Three-Way Tree & File Merging (`diff3`)**: How comparing $(B \to O)$ against $(B \to T)$ automatically resolves non-overlapping edits in the same file while isolating overlapping line ranges into conflict blocks (`<<<<<<<`, `=======`, `>>>>>>>`).
- **Multi-Stage Index Conflict Representation (`Stages 1, 2, 3`)**: How `.git/index` represents an unresolved conflict by removing stage `0` for that path and inserting up to three entries with the exact same path:
  - `Stage 1`: Base version ($B$)
  - `Stage 2`: Ours version ($O$, current `HEAD`)
  - `Stage 3`: Theirs version ($T$, branch being merged)
  And how `gir add <file>` resolves the conflict by moving the old stage `1/2/3` entries into the `REUC` (Resolve-Undo) index extension and writing stage `0`.
- **The Persistent `.git/sequencer` State Machine**: How multi-step operations (`cherry-pick`, `rebase`, `rebase -i`) persist their remaining todo queue to disk when paused on a merge conflict so `--continue`, `--skip`, and `--abort` work across shell sessions.

### Rust Concepts
- **Graph Reachability Coloring (`BitFlags` on DAG Nodes)**: Painting ancestor sets (`OURS_ANCESTOR = 0b01`, `THEIRS_ANCESTOR = 0b10`) to find maximal common ancestors cleanly.
- **Line-Range Interval Intersection**: Aligning two Myers edit scripts (`Base -> Ours` and `Base -> Theirs`) over shared base line coordinates $[b_{\text{start}}, b_{\text{end}})$ to emit clean merged chunks vs. conflict chunks.

---

## 2. Files to Create / Modify

```text
crates/gir-core/src/
├── merge/
│   ├── mod.rs              # merge_branch(repo, target_rev, no_ff, ff_only, abort)
│   ├── lca.rs              # find_merge_bases(odb, commit_a, commit_b) -> Vec<ObjectId>
│   ├── tree_merge.rs       # merge_trees_3way(odb, base_tree, ours_tree, theirs_tree)
│   └── file_merge.rs       # merge_blobs_3way(base, ours, theirs, labels) -> MergeFileResult
└── rebase/
    ├── mod.rs              # Sequencer engine (.git/rebase-merge & .git/sequencer)
    ├── cherry_pick.rs      # cherry_pick(repo, commits, continue, abort)
    └── interactive.rs      # TodoCommand (Pick, Reword, Edit, Squash, Fixup, Drop)
crates/gir-cli/src/commands/
├── merge.rs
├── cherry_pick.rs
└── rebase.rs
```

---

## 3. Algorithmic Specifications

### 3.1 Three-Way Tree Merge Matrix (`merge/tree_merge.rs`)
For every path present in `Base` ($B$), `Ours` ($O$), or `Theirs` ($T$):

| Base $B$ | Ours $O$ | Theirs $T$ | Resolution |
| :--- | :--- | :--- | :--- |
| $X$ | $X$ | $X$ | **Clean**: Keep $X$ (Stage 0) |
| $X$ | $X$ | $Y$ ($Y \ne X$) | **Clean**: Take $Y$ from Theirs (Stage 0) |
| $X$ | $Y$ ($Y \ne X$) | $X$ | **Clean**: Keep $Y$ from Ours (Stage 0) |
| $X$ | $Y$ | $Y$ | **Clean**: Both made identical change $Y$ (Stage 0) |
| $\text{None}$ | $\text{None}$ | $Y$ | **Clean**: Added by Theirs $\to$ Take $Y$ (Stage 0) |
| $\text{None}$ | $Y$ | $\text{None}$ | **Clean**: Added by Ours $\to$ Keep $Y$ (Stage 0) |
| $X$ | $X$ | $\text{None}$ | **Clean**: Deleted by Theirs, unmodified by Ours $\to$ Delete file |
| $X$ | $\text{None}$ | $X$ | **Clean**: Deleted by Ours, unmodified by Theirs $\to$ Keep deleted |
| $X$ | $Y$ ($Y \ne X$) | $\text{None}$ | **CONFLICT (Modify/Delete)**: Write stages `1=X, 2=Y` to index, keep `Y` in worktree |
| $X$ | $\text{None}$ | $Z$ ($Z \ne X$) | **CONFLICT (Delete/Modify)**: Write stages `1=X, 3=Z` to index, write `Z` to worktree |
| $X$ | $Y$ ($Y \ne X$) | $Z$ ($Z \ne X, Y$) | **Run 3-Way File Merge (`merge_blobs_3way(X, Y, Z)`)** |
| $\text{None}$ | $Y$ | $Z$ ($Z \ne Y$) | **Run 3-Way File Merge (`merge_blobs_3way(empty, Y, Z)`)** |

### 3.2 Three-Way File Merge (`merge/file_merge.rs`)
1. Compute Myers edit script $E_O = \text{diff}(B, O)$ and $E_T = \text{diff}(B, T)$, expressed as changed intervals on $B$: $([b_1, b_2), \text{replacement\_lines})$.
2. Walk base lines $0..|B|$:
   - If neither $E_O$ nor $E_T$ modifies the current span $\to$ emit base lines.
   - If only $E_O$ modifies $[b_1, b_2)$ and $E_T$ leaves $[b_1, b_2)$ unchanged $\to$ emit $E_O$'s replacement.
   - If only $E_T$ modifies $[b_1, b_2)$ and $E_O$ leaves $[b_1, b_2)$ unchanged $\to$ emit $E_T$'s replacement.
   - If both $E_O$ and $E_T$ modify overlapping base ranges:
     - If their replacement lines are identical $\to$ emit the replacement once (clean resolution!).
     - Otherwise $\to$ mark file `Conflicted` and emit standard conflict markers:
       ```text
       <<<<<<< HEAD
       <ours lines>
       =======
       <theirs lines>
       >>>>>>> <theirs_label>
       ```
       (and support `--conflict=diff3` which adds `||||||| base` between `<<<<<<<` and `=======`).

### 3.3 How `Cherry-Pick` and `Rebase` Reuse 3-Way Merge!
A cherry-pick of commit $C$ (whose parent is $P$) onto current `HEAD` $H$ is simply a **3-way merge** where:
- **Base ($B$)** = $P$'s tree (`C.parent.tree`)
- **Ours ($O$)** = $H$'s tree (`HEAD.tree`)
- **Theirs ($T$)** = $C$'s tree (`C.tree`)
If clean, `gir` creates a new commit on top of $H$ preserving $C$'s author and commit message!
`gir rebase <upstream>` simply collects the commits in `upstream..HEAD`, detaches `HEAD` at `upstream`, and executes this exact 3-way cherry-pick step for each commit in sequence.

---

## 4. Step-by-Step Implementation Tasks

### Task 7.1: Lowest Common Ancestor (`merge/lca.rs`)
1. Implement `find_merge_bases(odb: &ObjectDatabase, a: ObjectId, b: ObjectId) -> Result<Vec<ObjectId>, GirError>`.
2. Expose plumbing command `gir merge-base [--all] [--is-ancestor] <commit-a> <commit-b>`.

### Task 7.2: 3-Way File & Tree Merge + Conflict State (`merge/*`)
1. Capture pre-operation safety snapshot via `SafetySnapshotEngine::capture("merge")` before starting any merge.
2. Handle the 4 merge cases in `gir merge`:
   - **Already up to date**: `theirs` is an ancestor of `HEAD`.
   - **Fast-forward**: `HEAD` is an ancestor of `theirs` (and `--no-ff` is not set) $\to$ checkout `theirs` tree and advance branch ref.
   - **Clean 3-way merge**: all files merge cleanly $\to$ write merged index, write tree, create 2-parent merge commit `[HEAD, theirs]`, update branch ref.
   - **Conflicted 3-way merge**: write stages `1/2/3` into `.git/index` for conflicted files, writestage `0` for clean files, write conflict markers to working tree files, write `.git/MERGE_HEAD` (`<theirs-sha>\n`), `.git/ORIG_HEAD` (`<head-sha>\n`), and `.git/MERGE_MSG`.
3. Implement `gir merge --abort` (restoring `ORIG_HEAD` and clearing `.git/MERGE_*`) and `gir merge --continue`.

### Task 7.3: Cherry-Pick & Rebase Sequencer (`rebase/*`)
1. Capture pre-operation safety snapshot before `cherry-pick` or `rebase`.
2. Implement `gir cherry-pick (<commit>... | --continue | --abort | --skip)`.
3. Implement `gir rebase [-i | --interactive] [<upstream>] | --continue | --abort | --skip` using `.git/rebase-merge/` state files (`head-name`, `orig-head`, `onto`, `git-rebase-todo`, `done`) compatible with official `git` prompt scripts!

---

## 5. Differential Verification Suite

1. **`merge-base` Graph Test**: Construct a criss-cross merge graph and a standard diamond DAG. Assert `gir merge-base` returns the exact same ancestor SHA(s) as `/usr/bin/git merge-base`.
2. **Non-Overlapping Same-File 3-Way Merge Test**:
   - Branch `A` edits line 2 of `app.rs`. Branch `B` edits line 40 of `app.rs`.
   - Run `gir merge B` on branch `A`. Assert it cleanly auto-merges both edits into a 2-parent commit that passes `/usr/bin/git fsck --full`.
3. **Conflicted Merge Interop Test**:
   - Create a conflicting edit on line 10 of `app.rs` on both branches.
   - Run `gir merge B`. Assert exit code is non-zero, `app.rs` contains standard `<<<<<<<` / `=======` / `>>>>>>>` markers, and `/usr/bin/git status` recognizes the repo as being in an active merge with `both modified: app.rs` (stages 1, 2, 3 in `.git/index`)!
   - Resolve the file, run `gir add app.rs` and `gir commit -m "merged"`, and assert `MERGE_HEAD` is cleaned up and `REUC` is populated.
