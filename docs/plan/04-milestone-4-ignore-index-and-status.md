# Milestone 4: `.gitignore` Engine, Binary `.git/index` (DIRC) & Parallel `status`

**Parent Index**: [`00-index.md`](./00-index.md)  
**Target Crates**: `gir-core`, `gir-cli`  
**Commands Delivered**: `gir status`, `gir add`, `gir rm`, `gir mv`, `gir ls-files`  

---

## 1. What You Will Learn in This Milestone

### Systems Engineering Concepts
- **The `.git/index` Binary Cache (`DIRC`)**: Why Git is fast—storing a sorted binary array of every tracked file's POSIX `lstat` metadata (`ctime`, `mtime`, `dev`, `ino`, `mode`, `uid`, `gid`, `size`) alongside its 20-byte `ObjectId` so `git status` almost never reads file contents.
- **8-Byte Struct Alignment Padding in Binary Formats**: How DIRC v2 pads variable-length file paths with `1..=8` NUL bytes (`\0`) so every index entry is a multiple of 8 bytes.
- **Index Extensions (`TREE` Cache-Tree & `REUC` Resolve-Undo)**: How official `git` embeds optional extension chunks at the end of `.git/index`, and why modifying index entries without invalidating the `TREE` extension causes `git commit` to silently ignore staged files!
- **Hierarchical Globbing & Directory Pruning**: How `.gitignore` stacks rules as it descends directories, pruning entire subtrees like `node_modules/` or `target/` before touching their inodes.

### Rust Concepts
- **POSIX Metadata Extraction (`std::os::unix::fs::MetadataExt`)**: Accessing raw Linux inode numbers, nanosecond timestamps, and mode bits (`S_IFLNK`, `S_IXUSR`).
- **Data-Parallel Filesystem Traversal (`rayon`)**: Using `rayon::scope` / `par_iter` to stat working tree files across CPU cores while maintaining deterministic lexicographical output.

---

## 2. Files to Create / Modify

```text
crates/gir-core/src/
├── ignore/
│   ├── mod.rs              # IgnoreStack (hierarchical matcher across directory walk)
│   └── pattern.rs          # IgnoreRule (anchored, dir-only, negation, ** globs)
├── index/
│   ├── mod.rs              # GitIndex (in-memory sorted BTreeMap<(PathBString, u8), IndexEntry>)
│   ├── entry.rs            # IndexEntry, StatData, Stage (0=Normal, 1=Base, 2=Ours, 3=Theirs)
│   ├── codec.rs            # Binary DIRC v2/v3/v4 parser & writer + trailing SHA-1 checksum
│   └── extensions.rs       # CacheTree ('TREE') invalidator/serializer & ResolveUndo ('REUC')
└── status/
    └── mod.rs              # compute_status(repo) -> StatusReport (Staged, Unstaged, Untracked, Conflicted)
crates/gir-cli/src/commands/
├── status.rs
├── add.rs
├── rm.rs
├── mv.rs
└── ls_files.rs
```

---

## 3. Byte-Level Specifications

### 3.1 `.git/index` (DIRC v2 / v3) Binary Layout
All multi-byte integers are **Big-Endian**:

1. **12-Byte Header**:
   - `0..4`: Magic `b"DIRC"` (`0x44, 0x49, 0x52, 0x43`)
   - `4..8`: Version (`2` or `3` as `u32`)
   - `8..12`: Entry count `N` (`u32`)
2. **Sorted Index Entries ($N$ entries, sorted by `(path_bytes, stage)`)**:
   - `0..4`: `ctime_sec: u32`
   - `4..8`: `ctime_nsec: u32`
   - `8..12`: `mtime_sec: u32`
   - `12..16`: `mtime_nsec: u32`
   - `16..20`: `dev: u32`
   - `20..24`: `ino: u32`
   - `24..28`: `mode: u32` (`0o100644`, `0o100755`, `0o120000`, or `0o160000`)
   - `28..32`: `uid: u32`
   - `32..36`: `gid: u32`
   - `36..40`: `file_size: u32` (truncated to lower 32 bits)
   - `40..60`: `oid: [u8; 20]` (raw 20-byte SHA-1 of the staged blob)
   - `60..62`: `flags: u16`:
     - `bit 15`: `assume_valid` (`1` bit)
     - `bit 14`: `extended` (`1` bit, `0` in v2, `1` in v3 if 2 extra flag bytes follow)
     - `bits 12..=13`: `stage` (`0` = normal, `1` = base, `2` = ours, `3` = theirs)
     - `bits 0..=11`: `name_len` (`path.len().min(0x0FFF) as u16`)
   - `62..(62 + path.len())`: UTF-8/raw path bytes relative to repo root (no leading `./`, `/` separator).
   - **Null Padding (1 to 8 `0x00` bytes)**:
     - Total entry byte length (from `ctime_sec` through the padding) **must be a multiple of 8 bytes** and include **at least one** `0x00` terminator byte:
       $$\text{pad\_len} = 8 - ((62 + \text{path.len()}) \bmod 8)$$
       (for v2 where `extended == 0`).
3. **Optional Extensions (while remaining bytes $> 20$)**:
   - `0..4`: 4-byte ASCII signature (e.g., `b"TREE"`, `b"REUC"`, `b"UNTR"`).
   - `4..8`: `ext_size: u32`.
   - `8..(8 + ext_size)`: Extension payload.
   - **Rule**: If signature first byte is `'A'..='Z'` (uppercase), it is an **optional** extension that can be safely dropped or invalidated when entries change.
4. **20-Byte Trailer**:
   - SHA-1 checksum of all preceding bytes in `.git/index`.

### 3.2 Stat-Cache Comparison & "Racy Git" Protection
When `gir status` compares an `IndexEntry` against a working tree file's `lstat`:
1. If `mtime_sec`, `mtime_nsec`, `file_size`, `ino`, or `mode` differ $\rightarrow$ hash the working tree file to check if content actually changed.
2. **Racy Git Rule**: If `entry.mtime_sec >= index_file_mtime_sec`, a file might have been modified within the same filesystem timestamp granularity tick right after `.git/index` was written! In this case, **always** read and hash the file to verify its `ObjectId` against `entry.oid`.

---

## 4. Step-by-Step Implementation Tasks

### Task 4.1: Hierarchical `.gitignore` Engine (`ignore/*`)
1. Parse `.gitignore` lines:
   - Strip comments (`#`) and unescaped trailing spaces.
   - Leading `!` marks a negation rule (`negated = true`).
   - Trailing `/` marks `dir_only = true`.
   - Leading `/` or an internal `/` anchors the pattern to the directory containing that `.gitignore`.
   - Convert glob tokens (`*`, `?`, `[abc]`, `**/`, `/**`, `/**/`) into a fast compiled matcher.
2. Build `IgnoreStack`:
   - Loads `~/.config/git/ignore`, `.git/info/exclude`, and root `.gitignore`.
   - Pushes/pops child `.gitignore` files as the directory walker descends/ascends.
   - Evaluates rules in reverse order (last matching rule wins; note that an ignored parent directory cannot be un-ignored by a child `!` rule).

### Task 4.2: `.git/index` Binary Parser, Writer & `TREE` Invalidator (`index/*`)
1. Implement `GitIndex::read(git_dir: &Path) -> Result<Self, GirError>`:
   - Verifies trailing 20-byte SHA-1 checksum before parsing.
   - Parses header, $N$ entries (handling v2 and v3 extended flags), and extensions (`TREE`, `REUC`).
2. Implement `GitIndex::add_or_update(&mut self, path: &str, oid: ObjectId, metadata: &std::fs::Metadata)`:
   - Normalizes mode to `0o100755` (if `(mode & 0o111) != 0`), `0o120000` (if symlink), or `0o100644`.
   - Removes any conflict stages (`1..=3`) for `path` and inserts stage `0`.
   - Invalidates `path` and all its parent directories in the `TREE` cache-tree extension!
3. Implement `GitIndex::write(&self, git_dir: &Path) -> Result<(), GirError>`:
   - Acquires `LockFile` on `.git/index.lock`, serializes header + sorted entries + valid extensions + trailing 20-byte SHA-1, and commits atomically.

### Task 4.3: Parallel `status` Engine & Upstream Ahead/Behind Calculation (`status/mod.rs`)
1. Check current branch's upstream tracking config in `.git/config` (`[branch "<name>"] remote = ...`, `merge = ...`). If configured and `refs/remotes/<remote>/<branch>` exists, compute `(ahead_count, behind_count)` so `gir status` prints `Your branch is up to date with 'origin/main'` or `ahead of 'origin/main' by N commits`.
2. Flatten current `HEAD` commit's `Tree` into a sorted map `head_files: BTreeMap<String, (u32, ObjectId)>`.
3. Compare `head_files` vs `GitIndex` to compute **Staged Changes** (`Added`, `Modified`, `Deleted`, `Renamed` via 50% content similarity heuristic, and `Unmerged` for stages `1..=3`).
4. Walk working tree using `IgnoreStack` + `rayon` parallel stat/hash evaluation to compare `GitIndex` vs Working Tree, computing **Unstaged Changes** (`Modified`, `Deleted`) and **Untracked Files** (collapsing wholly untracked directories like `untracked_dir/` unless `-uall` is passed).

### Task 4.4: CLI Commands (`gir-cli`)
1. `gir status [-s | --short] [-b | --branch] [-u[<mode>]] [--ignored]`
2. `gir add [-A | --all | -u | --update | -f | <pathspec>...]`
3. `gir rm [--cached] [-f] [-r] <pathspec>...`
4. `gir mv <source> <destination>`
5. `gir ls-files [-s | --stage] [-c] [-o] [-i] [--exclude-standard]`

---

## 5. Differential Verification Suite

1. **Byte-for-Byte `.git/index` Interop Test**:
   - Initialize a repo with `gir init`, create 20 files across nested directories (including executable scripts and symlinks), run `gir add .`.
   - Run `/usr/bin/git status --porcelain=v1` and `/usr/bin/git ls-files --stage`. Assert `/usr/bin/git` reports zero unstaged deltas and identical index stages/modes/SHAs.
2. **`TREE` Extension Cache Invalidation Test**:
   - In a repo managed by `/usr/bin/git` (which populates the `TREE` cache extension in `.git/index`), modify one deeply nested file, stage it using `gir add <file>`, and then commit using `/usr/bin/git commit -m "test"`.
   - Assert `/usr/bin/git show --stat` includes the modified file (proving `gir add` properly invalidated the stale `TREE` extension!).
3. **`.gitignore` Torture Test**:
   - Test nested `.gitignore` files with `*.log`, `!important.log`, `build/`, `**/temp/**`. Assert `gir status --porcelain=v1` matches `/usr/bin/git status --porcelain=v1` line-for-line.
