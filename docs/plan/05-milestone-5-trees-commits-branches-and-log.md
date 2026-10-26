# Milestone 5: Trees, Commits, Branches, Tags & Commit Graph Traversal (`log` / `show`)

**Parent Index**: [`00-index.md`](./00-index.md)  
**Target Crates**: `gir-core`, `gir-cli`  
**Commands Delivered**: `gir write-tree`, `gir commit-tree`, `gir commit`, `gir branch`, `gir tag`, `gir log`, `gir show`  

---

## 1. What You Will Learn in This Milestone

### Systems Engineering Concepts
- **Bottom-Up Merkle Tree Construction (`write-tree`)**: How a flat, lexicographically sorted `.git/index` (`a/b/c.rs`, `a/d.rs`, `main.rs`) is recursively rolled up from leaf directories to the root `Tree` object, rebuilding the `TREE` cache extension in `.git/index` at the same time.
- **Root Commits vs. Incremental Commits vs. `--amend`**: How Git handles an unborn branch (`HEAD` pointing to `refs/heads/main` which does not yet exist on disk) versus normal single-parent commits, merge commits (`MERGE_HEAD`), and `--amend` (replacing the tip commit using its parents).
- **Directed Acyclic Graph (DAG) Topological & Priority-Queue Walkers**: How `git log` traverses merge histories without visiting shared ancestors twice, supporting chronological order, `--topo-order`, and ASCII branch graph rendering (`*`, `|`, `/`, `\`).

### Rust Concepts
- **Recursive Trie / Directory Builders**: Transforming flat path slices into an in-memory directory tree and hashing bottom-up.
- **Custom `Iterator` with `BinaryHeap`**: Implementing a lazy `RevWalk` iterator backed by a max-heap ordered by commit timestamp and a `HashSet<ObjectId>` of seen commits.

---

## 2. Files to Create / Modify

```text
crates/gir-core/src/
├── index/
│   └── tree_builder.rs     # write_index_to_tree(index, odb) -> ObjectId + updates TREE extension
├── commit_ops.rs           # create_commit(repo, msg, amend, all, allow_empty)
├── branch_ops.rs           # list_branches, create_branch, delete_branch, rename_branch
├── tag_ops.rs              # list_tags, create_lightweight_tag, create_annotated_tag, delete_tag
└── graph/
    ├── mod.rs              # RevWalk iterator (chronological & topological DAG walk)
    └── ascii_graph.rs      # Columnar ASCII commit graph renderer (* | / \)
crates/gir-cli/src/commands/
├── write_tree.rs
├── commit_tree.rs
├── commit.rs
├── branch.rs
├── tag.rs
├── log.rs
└── show.rs
```

---

## 3. Algorithmic Specifications

### 3.1 `write-tree` Algorithm (`index/tree_builder.rs`)
1. **Verify Unmerged State**: Scan `GitIndex`. If any entry has `stage != 0` (`1..=3`), immediately abort with `GirError::UnmergedIndexEntries`.
2. **Build Directory Hierarchy**:
   - Insert each stage-0 `IndexEntry` into a recursive `DirNode` structure by splitting `entry.path` on `'/'`.
3. **Bottom-Up Hashing**:
   - For each directory (post-order traversal from deepest subdirectories up to root):
     - Construct `TreeEntry` items for files (`100644`, `100755`, `120000`, `160000`) and child subdirectories (`40000` with child tree `ObjectId`).
     - Sort entries using Git's tree entry comparator (`name` for files, `name + "/"` for directories).
     - Serialize and write `Tree` object to `ObjectDatabase`.
     - Populate the corresponding node in the index's `TREE` cache-tree extension (`entry_count`, `subtree_count`, `tree_oid`).
4. Return the root `Tree` `ObjectId`.

### 3.2 `gir commit` Transaction Flow (`commit_ops.rs`)
1. If `-a` / `--all` is passed: auto-stage all modified and deleted tracked files in `GitIndex` and write `.git/index`.
2. Run `write_index_to_tree` to obtain `root_tree_oid`.
3. Determine parent commit(s):
   - If `--amend`: parents = current `HEAD` commit's parents.
   - Else if unborn `HEAD`: `parents = vec![]`.
   - Else: `parents = vec![head_oid]`, plus any `ObjectId`s listed in `.git/MERGE_HEAD` (if finishing a merge).
4. **Empty Commit Guard**: If `!allow_empty` and `parents.len() == 1` and `parent_commit.tree == root_tree_oid`, abort with `"nothing to commit, working tree clean"`.
5. Resolve `author` and `committer` `Signature`s from env / `.git/config` / `~/.gitconfig`.
6. Write `Commit` object to `ObjectDatabase`.
7. Atomically update target ref (the branch pointed to by symbolic `HEAD`, or `HEAD` directly if detached) using `RefStore::update_ref`, recording reflog message:
   - `"commit (initial): <subject>"` (if root commit)
   - `"commit (amend): <subject>"` (if amend)
   - `"commit (merge): <subject>"` (if `MERGE_HEAD` was present; also unlink `.git/MERGE_HEAD` and `.git/MERGE_MSG`)
   - `"commit: <subject>"` (normal commit)

### 3.3 `RevWalk` & ASCII Graph Renderer (`graph/*`)
- **Chronological Walk (Default)**:
  - Push starting commit(s) onto a `BinaryHeap<(i64, ObjectId)>` ordered by `commit.committer.timestamp`.
  - Pop highest-timestamp commit `c`, yield `c`, and push each unseen `parent` of `c` into the heap.
  - If an `excluded` boundary set is specified (e.g., `main..feature` excludes ancestors of `main`), propagate an `UNINTERESTING` mark down parent links until all active heap items are uninteresting.
- **ASCII Graph Renderer (`log --graph`)**:
  - Maintains an active list of `columns: Vec<ObjectId>` representing branch lanes flowing downward.
  - When commit `c` is popped at column `col`:
    - Renders `*` at `col` and `|` at all other active columns.
    - Replaces `columns[col]` with `c.parents[0]` (if any), and inserts `c.parents[1..]` into adjacent columns, drawing `/` and `\` connector lines when columns merge or fork.

---

## 4. Step-by-Step Implementation Tasks

### Task 5.1: `write-tree` & `commit-tree` Plumbing
1. Implement `write_index_to_tree(&mut GitIndex, &ObjectDatabase) -> Result<ObjectId, GirError>`.
2. Implement `gir write-tree` and `gir commit-tree <tree> [-p <parent>...] -m <msg>`.

### Task 5.2: `gir commit` Porcelain & Git Hooks Execution
1. Implement Git Hooks runner (`hooks.rs`): resolves hooks directory from `core.hooksPath` in `.git/config` (e.g., `.husky`) or defaults to `.git/hooks/`. Unless `-n` / `--no-verify` is passed:
   - Executes `pre-commit` before building the commit tree; aborts commit if exit code $\ne 0$.
   - Writes `.git/COMMIT_EDITMSG` and executes `commit-msg .git/COMMIT_EDITMSG`; aborts if exit code $\ne 0$.
   - Executes `post-commit` after ref update completes.
2. Implement `gir commit -m <msg> [-a] [-n | --no-verify] [--amend] [--allow-empty] [--author="<Name <email>>"]`.
3. Print standard summary output: `[<branch> (root-commit) <short-sha>] <subject>\n <N> files changed, <ins> insertions(+), <del> deletions(-)`.

### Task 5.3: Branch & Tag Management (`branch_ops.rs` & `tag_ops.rs`)
1. Implement `gir branch`:
   - Listing with `*` on current branch, `-v` with short SHA and subject, `-a` including remotes.
   - Creating `<new-branch> [<start-point>]`.
   - Deleting `-d` (safe delete checking merge reachability) and `-D` (force delete).
   - Renaming `-m` / `-M [<old-branch>] <new-branch>`:
     - **Unborn branch support**: If `HEAD` points to an unborn branch (e.g., before the first commit), updates `.git/HEAD` to `ref: refs/heads/<new-branch>`.
     - **Committed branch support**: Atomically creates `refs/heads/<new-branch>`, moves `.git/logs/refs/heads/<old>` $\rightarrow$ `<new>` (appending `"Branch: renamed refs/heads/<old> to refs/heads/<new>"`), updates `.git/HEAD` if `<old>` was checked out, renames `[branch "<old>"]` to `[branch "<new>"]` in `.git/config`, and deletes `refs/heads/<old>` (matching `git branch -M main` 1:1).
2. Implement `gir tag` (listing sorted lexicographically, lightweight tag creation, `-a -m <msg>` annotated `Tag` object creation, `-d` delete).

### Task 5.4: `gir log` & `gir show`
1. Implement `gir log [-n <limit>] [--oneline] [--graph] [--stat] [<revision-range>]`.
2. Implement `gir show [<object>]` displaying commit metadata + tree diff against parent[0] (for commits), tag header + target (for tags), tree listing (for trees), or blob content (for blobs).

---

## 5. Differential Verification Suite

1. **`write-tree` & `commit-tree` Exact Hash Match**:
   - Stage a multi-directory project in two identical temp directories—one using `/usr/bin/git`, one using `gir`.
   - Assert `gir write-tree` outputs the **exact same 40-char SHA-1** as `/usr/bin/git write-tree`.
   - With pinned `GIT_AUTHOR_DATE` and `GIT_COMMITTER_DATE`, assert `gir commit -m "init"` creates the **exact same commit SHA-1** as `/usr/bin/git commit -m "init"`, and `/usr/bin/git fsck --full` succeeds.
2. **`log` Range & Graph Order Test**:
   - On a repository with branches and merge commits, verify `gir log --oneline main..feature` outputs the exact same sequence of commits as `/usr/bin/git log --oneline main..feature`.
