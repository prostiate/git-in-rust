# Milestone 2: Unified Packfile Engine (`.idx` v2 + `.pack` Reader, Delta Decoder & Pack Writer)

**Parent Index**: [`00-index.md`](./00-index.md)  
**Target Crates**: `gir-core`, `gir-cli`  
**Commands Delivered**: `gir index-pack`, `gir unpack-objects`, `gir pack-objects`, `gir fsck`, `gir gc`  

---

## 1. What You Will Learn in This Milestone

### Systems Engineering Concepts
- **Memory-Mapped Binary Indices (`mmap`)**: How Git looks up an object among 1,000,000+ packed objects in $<5\ \mu\text{s}$ without loading the index into heap RAM, using a 256-entry first-level fanout table and $O(\log N)$ binary search over contiguous 20-byte SHA arrays.
- **Bit-Packed Variable-Length Integers (Varints)**: How `.pack` files encode object types, uncompressed sizes, and negative relative offsets (`OBJ_OFS_DELTA`) using MSB continuation bits.
- **Delta Compression VM**: How Git stores 100 versions of a source file in a fraction of the space by encoding a bytecode stream of **COPY (offset, size)** and **INSERT (literal bytes)** instructions against a base object.
- **Sliding-Window Delta Heuristics**: How `pack-objects` sorts objects by `(ObjectKind, filename_hash, size)` and uses Rabin-Karp / rolling hashes over a sliding window to find optimal delta bases.

### Rust Concepts
- **Safe Encapsulation of `unsafe` (`memmap2::Mmap`)**: Wrapping OS memory maps in safe Rust structs that validate file bounds and magic headers upfront so slice accesses (`&mmap[offset..offset + 20]`) are guaranteed panic-free and zero-copy.
- **Bitwise Manipulation**: Writing clean, well-tested bit shift/mask decoders for Git's 3 different varint encodings (object header varint, `OFS_DELTA` base offset varint, and delta instruction stream varint).
- **Bounded LRU Caching**: Using an in-memory cache (`lru` or custom `HashMap` + doubly-linked list / generation counter) keyed by `(pack_id, pack_offset)` to avoid re-inflating base objects during deep delta chains.

---

## 2. Files to Create / Modify

```text
crates/gir-core/src/
├── pack/
│   ├── mod.rs
│   ├── index.rs            # PackIndexV2 (.idx v2 mmap reader & writer)
│   ├── reader.rs           # PackFile (.pack mmap reader, header parser, entry decoder)
│   ├── delta.rs            # apply_delta(base, delta) & compute_delta(base, target)
│   └── writer.rs           # PackWriter (pack-objects & index-pack builder)
└── storage/
    ├── mod.rs              # Unified ObjectDatabase (LooseObjectStore + Vec<PackBundle>)
    └── fsck.rs             # Full repository integrity verifier (loose + packed + connectivity)
crates/gir-cli/src/commands/
├── index_pack.rs
├── unpack_objects.rs
├── pack_objects.rs
└── fsck.rs
```

---

## 3. Byte-Level Specifications

### 3.1 `.idx` Version 2 Binary Format
All integers are **Big-Endian (`u32::from_be_bytes`)**:
1. **Header (8 bytes)**:
   - Magic: `0xFF, 0x74, 0x4F, 0x63` (`"\xfftOc"`)
   - Version: `0x00, 0x00, 0x00, 0x02` (`2u32`)
2. **Fanout Table (256 $\times$ 4 bytes = 1024 bytes)**:
   - Entry `i` (`0..=255`) stores the total count of objects in the pack whose SHA-1 first byte is $\le i$.
   - Total objects in pack $N = \text{fanout}[255]$.
   - For a target `sha`, let $b = \text{sha}[0]$. The search range in the SHA table is:
     $$\text{lo} = \begin{cases} 0 & \text{if } b = 0 \\ \text{fanout}[b - 1] & \text{if } b > 0 \end{cases}, \quad \text{hi} = \text{fanout}[b]$$
3. **SHA-1 Table ($N \times 20$ bytes)**: Sorted array of 20-byte raw `ObjectId`s.
4. **CRC32 Table ($N \times 4$ bytes)**: CRC32 checksum of the packed zlib data for each object.
5. **32-bit Offset Table ($N \times 4$ bytes)**:
   - If MSB (`bit 31`) is `0`: bits `0..31` are the direct byte offset in `.pack`.
   - If MSB (`bit 31`) is `1`: bits `0..31` are an index into the **64-bit Extended Offset Table**.
6. **64-bit Extended Offset Table ($M \times 8$ bytes)**: Present only if any offset $\ge 2\text{ GiB}$.
7. **Trailers (40 bytes)**:
   - 20-byte SHA-1 checksum of the corresponding `.pack` file.
   - 20-byte SHA-1 checksum of all preceding bytes in this `.idx` file.

### 3.2 `.pack` Version 2 Format & Variable-Length Headers
1. **Header (12 bytes)**:
   - Magic: `b"PACK"` (4 bytes)
   - Version: `2u32` big-endian (4 bytes)
   - Object Count: `N` (`u32` big-endian, 4 bytes)
2. **Entry Header (Variable Length)**:
   - First byte `b0`:
     - `bit 7` (`0x80`): MSB continuation flag (`1` = more header bytes follow).
     - `bits 4..=6` (`(b0 >> 4) & 0x07`): Object Type:
       - `1` = `OBJ_COMMIT`
       - `2` = `OBJ_TREE`
       - `3` = `OBJ_BLOB`
       - `4` = `OBJ_TAG`
       - `6` = `OBJ_OFS_DELTA`
       - `7` = `OBJ_REF_DELTA`
     - `bits 0..=3` (`b0 & 0x0F`): Lowest 4 bits of uncompressed size.
   - Subsequent bytes `bi` (while previous byte had `bit 7 == 1`):
     - `bits 0..=6` (`bi & 0x7F`) shifted left by `4 + (i - 1) * 7`.
3. **Delta Base Specifier (immediately after Entry Header, before zlib stream)**:
   - For `OBJ_REF_DELTA` (`7`): 20-byte raw base `ObjectId`.
   - For `OBJ_OFS_DELTA` (`6`): Variable-length **negative offset** from the start of the current pack entry:
     ```rust
     let mut b = read_u8()?;
     let mut neg_offset = (b & 0x7F) as u64;
     while (b & 0x80) != 0 {
         neg_offset += 1; // Critical Git offset-encoding bias!
         b = read_u8()?;
         neg_offset = (neg_offset << 7) | ((b & 0x7F) as u64);
     }
     let base_entry_offset = current_entry_offset - neg_offset;
     ```

### 3.3 Delta Bytecode Instruction Format (`delta.rs`)
After inflating the zlib stream of a delta entry:
1. Read source (base) size (standard 7-bit little-endian varint).
2. Read target (result) size (standard 7-bit little-endian varint).
3. Loop over remaining instruction bytes `cmd`:
   - **If `cmd & 0x80 != 0` (COPY from base object)**:
     - Bits `0..=3` indicate which of the 4 little-endian offset bytes follow (`offset` up to `u32`).
     - Bits `4..=6` indicate which of the 3 little-endian size bytes follow (`size` up to `u24`).
     - Special case: if decoded `size == 0`, set `size = 0x10000` (65536 bytes).
     - Append `&base[offset..offset + size]` to target buffer.
   - **If `cmd & 0x80 == 0 && cmd != 0` (INSERT literal bytes)**:
     - `size = (cmd & 0x7F) as usize`.
     - Append the next `size` bytes directly from the delta stream to the target buffer.
   - **If `cmd == 0`**: Reserved/invalid opcode; return `GirError::CorruptPackDelta`.

---

## 4. Step-by-Step Implementation Tasks

### Task 2.1: `.idx` v2 Memory-Mapped Reader (`pack/index.rs`)
1. Implement `PackIndexV2::open(path: &Path) -> Result<Self, GirError>`:
   - Memory-map the `.idx` file via `unsafe { Mmap::map(&file)? }`.
   - Verify magic `\xfftOc`, version `2`, minimum length, and table bounds.
2. Implement `PackIndexV2::lookup(&self, id: &ObjectId) -> Option<PackIndexEntry>` returning `{ pack_offset: u64, crc32: u32 }`.
3. Implement `PackIndexV2::resolve_prefix(&self, hex_prefix: &str) -> Vec<ObjectId>` using the fanout table + binary search for fast short-SHA lookup.

### Task 2.2: `.pack` Reader & Delta Reconstructor (`pack/reader.rs` & `pack/delta.rs`)
1. Implement `apply_delta(base: &[u8], delta_stream: &[u8]) -> Result<Vec<u8>, GirError>`.
2. Implement `PackFile::open(path: &Path) -> Result<Self, GirError>`:
   - Validates `PACK` header, version `2`, and trailing 20-byte SHA-1.
   - Includes a thread-safe LRU base cache (`Mutex<LruCache<u64, RawObject>>`, default 256 entries / 64 MB) so delta chains resolve in $O(1)$ amortized reads.
3. Implement `PackFile::read_object_at(&self, offset: u64, odb: &ObjectDatabase) -> Result<RawObject, GirError>`.
   - Notice how zlib streams inside `.pack` files do not store their compressed length: use `flate2::Decompress` (or `ZlibDecoder` wrapping a slice cursor) and extract `decompressor.total_in()` to know exact compressed byte consumption!

### Task 2.3: Unified `ObjectDatabase` (`storage/mod.rs`)
1. Combine `LooseObjectStore` and discovered `PackBundle` pairs (`.idx` + `.pack` in `.git/objects/pack/`) inside `ObjectDatabase`.
2. Implement `ObjectDatabase::read(&self, id: &ObjectId) -> Result<RawObject, GirError>`:
   - Checks loose object store $\rightarrow$ checks loaded packfiles $\rightarrow$ if missing, re-scans `.git/objects/pack/` once (in case a concurrent `git fetch` or `gc` added a new pack) before returning `GirError::ObjectNotFound`.
3. Implement `ObjectDatabase::resolve_prefix(&self, prefix: &str) -> Result<ObjectId, GirError>`:
   - Dedupes matches across loose and packed stores; returns `GirError::AmbiguousObjectPrefix` if $>1$ match.

### Task 2.4: Pack Writer, Delta Compressor & `index-pack` (`pack/writer.rs`)
1. Implement `compute_delta(base: &[u8], target: &[u8]) -> Option<Vec<u8>>` using 16-byte rolling hash chunks to emit COPY and INSERT opcodes when savings $> 10\%$.
2. Implement `PackWriter`: writes `PACK` header, encodes objects (with `OFS_DELTA` against recent same-type objects in a 10-entry sliding window), computes CRC32 per object, appends 20-byte pack SHA-1, and writes the companion `.idx` v2 file atomically.
3. Implement `index_pack(pack_path: &Path) -> Result<PathBuf, GirError>`: streams an existing `.pack` file (e.g., received from network fetch), verifies all objects/deltas and CRC32s, and builds its `.idx` v2 index.

### Task 2.5: CLI Commands (`gir-cli`)
1. `gir index-pack <pack-file>`
2. `gir unpack-objects < <pack-file>`
3. `gir pack-objects <base-name> < <object-list>`
4. `gir fsck [--full]`
5. `gir gc [--prune]`: packs reachable loose objects (including `refs/gir/snapshots/*` so safety snapshots remain protected) into `.git/objects/pack/pack-<sha>.{pack,idx}`, packs loose refs into `.git/packed-refs`, and unlinks packed loose object files.

---

## 5. Differential Verification Suite

1. **Real Packed Repo Test**: Clone or copy a real repository with packed history, run `/usr/bin/git gc --aggressive` on it so 100% of objects are inside `.git/objects/pack/*.pack` with deep `OFS_DELTA` chains. Verify `gir cat-file -p <sha>` and `gir fsck --full` read every single commit, tree, and blob identically to `/usr/bin/git`.
2. **Round-Trip Pack Generation Test**: Generate a packfile using `gir pack-objects`, then run `/usr/bin/git verify-pack -v <pack>.idx` on it. Assert `/usr/bin/git` validates every CRC32, offset, and delta chain with exit code `0`.
3. **`index-pack` Byte-for-Byte Test**: Take a `.pack` file generated by `/usr/bin/git pack-objects`, build its `.idx` using `gir index-pack`, and assert the resulting `.idx` file is **100% byte-for-byte identical** to the `.idx` produced by `/usr/bin/git index-pack`.
