# Milestone 8: Smart HTTP/SSH Wire Transport (`gir-transport`) & Interactive TUI (`gir-tui`)

**Parent Index**: [`00-index.md`](./00-index.md)  
**Target Crates**: `gir-transport`, `gir-tui`, `gir-cli`  
**Commands Delivered**: `gir remote`, `gir clone`, `gir fetch`, `gir pull`, `gir push`, `gir tui`  

---

## 1. What You Will Learn in This Milestone

### Systems Engineering Concepts
- **Git's `pkt-line` Wire Framing Protocol**: How Git frames variable-length control lines and multiplexed binary streams using 4-byte ASCII hexadecimal length prefixes (`0000` flush-pkt, `0001` delimiter-pkt, `0004..ffff` payload frames).
- **Smart HTTP & SSH Pack Negotiation (`upload-pack` & `receive-pack`)**:
  - How `clone` and `fetch` discover remote references, negotiate common ancestor commits via `want` / `have` / `ACK` / `NAK` packets so only missing objects are transmitted, and demultiplex `side-band-64k` streams (`band 1` = packfile data, `band 2` = remote progress stderr, `band 3` = fatal error).
  - How `push` sends atomic ref-update commands (`<old-sha> <new-sha> <refname>\0report-status side-band-64k`) followed immediately by a delta-compressed `.pack` stream.
- **Reusable Server-Side Protocol Handlers for `GirHub`**: Designing `gir-transport` so it contains both the **client** (`clone`/`fetch`/`push`) and the **server** (`handle_info_refs`, `handle_upload_pack`, `handle_receive_pack`) state machines that `GirHub` will mount directly onto its HTTP/SSH endpoints!
- **Zero-Overhead Immediate-Mode Terminal UI (`ratatui`)**: Building an ultra-fast keyboard-driven Git TUI that calls `gir-core` directly in-memory (0ms subprocess latency, instant hunk staging, live DAG graph, and 1-key `gir undo` time-travel).

---

## 2. Files to Create / Modify

```text
crates/gir-transport/src/
├── lib.rs
├── pktline.rs              # PktLineReader & PktLineWriter (4-hex length framing + flush/delim)
├── sideband.rs             # Sideband-64k multiplexer & demultiplexer (bands 1, 2, 3)
├── refs_discovery.rs       # RemoteRef & Capability parsing (ofs-delta, side-band-64k, report-status)
├── client/
│   ├── mod.rs              # TransportClient trait (SmartHttpTransport, SshTransport, LocalTransport)
│   ├── http.rs             # GET /info/refs?service=..., POST /git-upload-pack & /git-receive-pack
│   ├── ssh.rs              # SSH channel spawning git-upload-pack / git-receive-pack
│   ├── fetch.rs            # clone & fetch negotiation state machine -> writes .pack + index-pack
│   └── push.rs             # reachable object walker -> pack-objects -> receive-pack + report-status
└── server/                 # Directly reusable by GirHub!
    ├── mod.rs
    ├── upload_pack.rs      # Server-side git-upload-pack handler against a gir-core Repository
    └── receive_pack.rs     # Server-side git-receive-pack handler (atomic ref updates + unpack/index-pack)

crates/gir-tui/src/
├── lib.rs                  # run_tui(repo: Repository) -> Result<(), GirError>
├── app.rs                  # AppState, active tab/pane, keymap dispatcher
├── theme.rs                # High-contrast terminal color palette
└── views/
    ├── mod.rs
    ├── status_pane.rs      # Staged / Unstaged / Untracked files (Space to stage/unstage, 'c' commit)
    ├── diff_pane.rs        # Syntax-colored unified/split diff & hunk-by-hunk staging ('s' stage hunk)
    ├── log_graph_pane.rs   # Interactive commit DAG graph + commit file-tree inspector
    ├── branches_pane.rs    # Branches, remotes, tags, stashes (checkout, merge, push/pull)
    └── snapshots_pane.rs   # Pre-Operation Safety Snapshots & Reflog time-machine ('u' instant undo)

crates/gir-cli/src/commands/
├── remote.rs
├── clone.rs
├── fetch.rs
├── pull.rs
├── push.rs
└── tui.rs
```

---

## 3. Wire Protocol Specifications

### 3.1 `pkt-line` Framing (`pktline.rs`)
- **Special packets**:
  - `b"0000"`: Flush packet (`PktLine::Flush`) — marks the end of a message section.
  - `b"0001"`: Delimiter packet (`PktLine::Delimiter`).
  - `b"0002"`: Response end packet (`PktLine::ResponseEnd`).
- **Data packets**:
  - 4-byte ASCII hex length `L` (`0004` to `fff0`), where `L` **includes the 4 length bytes themselves** (`payload.len() + 4`).
  - Example: `"want <40-hex-sha>\n"` (46 bytes payload) $\to$ length $46 + 4 = 50 = \text{0x0032}$ $\to$ `b"0032want <40-hex-sha>\n"`.

### 3.2 Smart HTTP `fetch` / `clone` Handshake (`client/fetch.rs`)
1. **Discovery**:
   - `GET <repo_url>/info/refs?service=git-upload-pack`
   - Verify first pkt-line is `# service=git-upload-pack\n` followed by `0000`.
   - Parse advertised refs:
     - First ref line: `<40-hex-sha> <refname>\0<space-separated-capabilities>\n` (including `symref=HEAD:refs/heads/main`, `side-band-64k`, `ofs-delta`, `multi_ack_detailed`).
     - Subsequent lines: `<40-hex-sha> <refname>\n` until `0000`.
2. **Negotiation (`POST <repo_url>/git-upload-pack`)**:
   - Send `want <oid> <capabilities>\n` for each target remote ref not already in local `ObjectDatabase`.
   - Send `0000` (flush).
   - Send `have <oid>\n` for local branch tips, followed by `done\n` and `0000`.
3. **Pack Reception & Indexing**:
   - Demultiplex `side-band-64k` packets:
     - Byte `0x01`: append payload bytes to incoming `.git/objects/pack/tmp_pack_XXXXXX`.
     - Byte `0x02`: print remote progress (`Counting objects...`, `Compressing objects...`) to terminal `stderr`.
     - Byte `0x03`: abort with remote error string.
   - Run `index_pack` on the received `.pack` file to verify all objects/deltas and build the `.idx` v2 file.
   - Update `.git/refs/remotes/<remote>/*` and `.git/FETCH_HEAD`.

### 3.3 Smart HTTP `push` Handshake (`client/push.rs`)
1. **Discovery**: `GET <repo_url>/info/refs?service=git-receive-pack`.
2. **Object Enumeration**:
   - Walk local commit DAG from `<local_oid>` stopping at any commit reachable from advertised remote refs.
   - Collect all reachable commits, trees, and blobs missing on the remote.
3. **Transmission (`POST <repo_url>/git-receive-pack`)**:
   - Write command line: `<40-hex-old-remote-oid> <40-hex-new-local-oid> refs/heads/<branch>\0 report-status side-band-64k ofs-delta\n` followed by `0000`.
   - Stream generated `.pack` bytes directly after the `0000` flush packet.
   - Parse `unpack ok\n` and `ok refs/heads/<branch>\n` from the response.

---

## 4. Step-by-Step Implementation Tasks

### Task 8.1: `pkt-line` & `sideband` Codecs (`gir-transport`)
1. Implement `PktLineReader` and `PktLineWriter` with comprehensive unit tests for edge lengths (`0000`, `0004`, max `65520`).
2. Implement `SidebandReader` and `SidebandWriter`.

### Task 8.2: Client & Server `upload-pack` / `receive-pack` + Credential Helper (`gir-transport`)
1. Implement **Git Credential Helper Protocol (`credentials.rs`)**:
   - Reads `credential.helper` from `~/.gitconfig` and `.git/config` (e.g., `gh auth git-credential`, `store`, `cache`).
   - On HTTP `401 Unauthorized` (or proactively when credentials exist), invokes `<helper> get` passing `protocol=https\nhost=<host>\npath=<repo>\n\n` on `stdin`, parses `username=...` and `password=...` from `stdout`, and attaches HTTP Basic Auth header.
   - Calls `<helper> approve` on `200 OK` or `<helper> reject` on `401/403`—so pushing to `https://github.com/prostiate/...` works automatically with your existing GitHub login!
2. Implement `SmartHttpTransport` and `SshTransport` (using system `ssh` / `SSH_AUTH_SOCK` agent forwarding to spawn `git-upload-pack` / `git-receive-pack`).
3. Implement server-side `handle_upload_pack` and `handle_receive_pack` so we can test `gir clone`, `gir fetch`, and `gir push` locally in integration tests and reuse them directly in `GirHub`!

### Task 8.3: Remote CLI Commands, Upstream Tracking & `pre-push` Hook (`gir-cli`)
1. `gir remote [-v | add <name> <url> | remove <name> | set-url <name> <url>]`
2. `gir clone <url> [<directory>]`: initializes repo, configures `remote.origin.url` and `remote.origin.fetch`, fetches packfile, sets `branch.<default>.remote = origin` and `branch.<default>.merge = refs/heads/<default>`, and checks out the default branch.
3. `gir fetch [<remote> [<refspec>...]]`
4. `gir pull [--rebase | --ff-only] [<remote> [<branch>]]`: defaults to current branch's configured `[branch "<name>"]` upstream if `<remote>` and `<branch>` are omitted!
5. `gir push [-u | --set-upstream] [-f | --force-with-lease] [--no-verify] [<remote> [<refspec>...]]`:
   - Executes `pre-push` hook (`core.hooksPath` or `.git/hooks/pre-push`) passing `<local_ref> <local_sha> <remote_ref> <remote_sha>\n` on `stdin` before network transmission (unless `--no-verify`).
   - When `-u` / `--set-upstream` is passed, writes `[branch "<name>"] remote = <remote>` and `merge = refs/heads/<branch>` into `.git/config` and prints `branch '<name>' set up to track '<remote>/<branch>'.`—matching `git push -u origin main` 1:1!

### Task 8.4: Interactive Terminal UI (`gir-tui` & `gir tui`)
1. Implement 5-pane keyboard-driven layout (`1` Status/Files, `2` Branches/Remotes/Stashes, `3` Commit Graph Log, `4` Safety Snapshots & Reflog Time-Machine, `Tab` to focus Diff/Hunk Stage viewer):
   - `Space`: Stage / unstage selected file or hunk.
   - `c`: Open inline commit message modal & commit.
   - `p` / `P`: Pull / Push with live sideband progress bar.
   - `u`: Instant `gir undo` with confirmation preview showing exact files restored.
   - `q` / `Esc`: Cleanly restore terminal alternate screen.

---

## 5. Differential Verification Suite

1. **Cross-Binary Clone/Push/Pull Interop Test**:
   - Create a bare repository on disk (`git init --bare remote.git`).
   - Run `gir clone remote.git repo-a`, create commits in `repo-a` using `gir`, and run `gir push origin main`.
   - Clone `remote.git` into `repo-b` using `/usr/bin/git clone remote.git repo-b`, add a commit using `/usr/bin/git`, and push via `/usr/bin/git push`.
   - Back in `repo-a`, run `gir pull origin main`. Assert both repositories have identical `HEAD` SHAs, working trees, and pass `git fsck --full`.
2. **Real GitHub Clone Test**:
   - Run `gir clone` against a real HTTPS repository and verify `.git/index`, working tree files, `.git/refs/remotes/origin/*`, and `gir log` match `/usr/bin/git`.
