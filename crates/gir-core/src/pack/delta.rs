use std::collections::HashMap;

use crate::error::GirError;

const CHUNK_LEN: usize = 16;

/// Applies a Git packfile delta instruction stream to `base`, reconstructing the target object payload.
pub fn apply_delta(base: &[u8], delta: &[u8]) -> Result<Vec<u8>, GirError> {
    let mut pos = 0;

    let expected_base_size = read_size_varint(delta, &mut pos)?;
    if base.len() != expected_base_size {
        return Err(GirError::CorruptObject {
            id: "<delta>".to_string(),
            reason: format!(
                "delta base size mismatch: expected {expected_base_size}, actual {}",
                base.len()
            ),
        });
    }

    let target_size = read_size_varint(delta, &mut pos)?;
    let mut out = Vec::with_capacity(target_size);

    while pos < delta.len() {
        let cmd = delta[pos];
        pos += 1;

        if (cmd & 0x80) != 0 {
            // COPY instruction from `base`
            let mut copy_offset: usize = 0;
            let mut copy_size: usize = 0;

            if (cmd & 0x01) != 0 {
                copy_offset |= read_byte(delta, &mut pos)? as usize;
            }
            if (cmd & 0x02) != 0 {
                copy_offset |= (read_byte(delta, &mut pos)? as usize) << 8;
            }
            if (cmd & 0x04) != 0 {
                copy_offset |= (read_byte(delta, &mut pos)? as usize) << 16;
            }
            if (cmd & 0x08) != 0 {
                copy_offset |= (read_byte(delta, &mut pos)? as usize) << 24;
            }

            if (cmd & 0x10) != 0 {
                copy_size |= read_byte(delta, &mut pos)? as usize;
            }
            if (cmd & 0x20) != 0 {
                copy_size |= (read_byte(delta, &mut pos)? as usize) << 8;
            }
            if (cmd & 0x40) != 0 {
                copy_size |= (read_byte(delta, &mut pos)? as usize) << 16;
            }

            if copy_size == 0 {
                copy_size = 0x10000;
            }

            let Some(end_offset) = copy_offset.checked_add(copy_size) else {
                return Err(GirError::CorruptObject {
                    id: "<delta>".to_string(),
                    reason: "delta COPY offset overflow".to_string(),
                });
            };
            if end_offset > base.len() {
                return Err(GirError::CorruptObject {
                    id: "<delta>".to_string(),
                    reason: format!(
                        "delta COPY range {copy_offset}..{end_offset} exceeds base length {}",
                        base.len()
                    ),
                });
            }

            out.extend_from_slice(&base[copy_offset..end_offset]);
        } else if cmd != 0 {
            // INSERT instruction from literal delta stream
            let insert_len = (cmd & 0x7f) as usize;
            if pos + insert_len > delta.len() {
                return Err(GirError::CorruptObject {
                    id: "<delta>".to_string(),
                    reason: "delta INSERT truncated".to_string(),
                });
            }
            out.extend_from_slice(&delta[pos..pos + insert_len]);
            pos += insert_len;
        } else {
            return Err(GirError::CorruptObject {
                id: "<delta>".to_string(),
                reason: "invalid delta opcode 0x00".to_string(),
            });
        }
    }

    if out.len() != target_size {
        return Err(GirError::CorruptObject {
            id: "<delta>".to_string(),
            reason: format!(
                "delta reconstructed size mismatch: expected {target_size}, got {}",
                out.len()
            ),
        });
    }

    Ok(out)
}

/// Computes a compressed Git delta instruction stream from `base` to `target`.
/// Returns `Some(delta_bytes)` if the delta is smaller than `target`, or `None` otherwise.
pub fn compute_delta(base: &[u8], target: &[u8]) -> Option<Vec<u8>> {
    if base.len() < CHUNK_LEN || target.len() < CHUNK_LEN {
        return None;
    }

    // Index 16-byte chunks of `base`
    let mut table: HashMap<u64, usize> = HashMap::with_capacity(base.len() / CHUNK_LEN + 1);
    let mut b_idx = 0;
    while b_idx + CHUNK_LEN <= base.len() {
        let h = hash_chunk(&base[b_idx..b_idx + CHUNK_LEN]);
        table.entry(h).or_insert(b_idx);
        b_idx += CHUNK_LEN / 2; // 8-byte stride for denser matching
    }

    let mut delta = Vec::new();
    write_size_varint(&mut delta, base.len());
    write_size_varint(&mut delta, target.len());

    let mut t_pos = 0;
    let mut pending_insert_start = 0;

    while t_pos + CHUNK_LEN <= target.len() {
        let h = hash_chunk(&target[t_pos..t_pos + CHUNK_LEN]);
        if let Some(&b_pos) = table.get(&h) {
            if base[b_pos..b_pos + CHUNK_LEN] == target[t_pos..t_pos + CHUNK_LEN] {
                // Extend match forward as far as possible
                let mut match_len = CHUNK_LEN;
                while b_pos + match_len < base.len()
                    && t_pos + match_len < target.len()
                    && base[b_pos + match_len] == target[t_pos + match_len]
                {
                    match_len += 1;
                }

                // Flush pending INSERT literals before emitting COPY
                if pending_insert_start < t_pos {
                    emit_insert(&mut delta, &target[pending_insert_start..t_pos]);
                }

                emit_copy(&mut delta, b_pos, match_len);
                t_pos += match_len;
                pending_insert_start = t_pos;

                // Abort early if delta is already larger than target
                if delta.len() >= target.len() {
                    return None;
                }
                continue;
            }
        }
        t_pos += 1;
    }

    if pending_insert_start < target.len() {
        emit_insert(&mut delta, &target[pending_insert_start..]);
    }

    // Only keep the delta if it saves at least 10% (and at least 16 bytes) over raw target
    if delta.len() + 16 < target.len() && delta.len() * 10 < target.len() * 9 {
        Some(delta)
    } else {
        None
    }
}

#[inline]
fn hash_chunk(chunk: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in chunk {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn emit_insert(out: &mut Vec<u8>, mut literals: &[u8]) {
    while !literals.is_empty() {
        let chunk_len = literals.len().min(0x7f);
        out.push(chunk_len as u8);
        out.extend_from_slice(&literals[..chunk_len]);
        literals = &literals[chunk_len..];
    }
}

fn emit_copy(out: &mut Vec<u8>, mut offset: usize, mut size: usize) {
    while size > 0 {
        let chunk_size = size.min(0x10000);
        let mut cmd = 0x80u8;
        let mut params = [0u8; 7];
        let mut param_len = 0;

        // Encode 32-bit offset bytes
        let off32 = offset as u32;
        if (off32 & 0x0000_00ff) != 0 {
            cmd |= 0x01;
            params[param_len] = (off32 & 0xff) as u8;
            param_len += 1;
        }
        if (off32 & 0x0000_ff00) != 0 {
            cmd |= 0x02;
            params[param_len] = ((off32 >> 8) & 0xff) as u8;
            param_len += 1;
        }
        if (off32 & 0x00ff_0000) != 0 {
            cmd |= 0x04;
            params[param_len] = ((off32 >> 16) & 0xff) as u8;
            param_len += 1;
        }
        if (off32 & 0xff00_0000) != 0 {
            cmd |= 0x08;
            params[param_len] = ((off32 >> 24) & 0xff) as u8;
            param_len += 1;
        }

        // Encode 24-bit size bytes (0x10000 is implicitly encoded by omitting all size bits)
        if chunk_size != 0x10000 {
            let sz32 = chunk_size as u32;
            if (sz32 & 0x0000_00ff) != 0 {
                cmd |= 0x10;
                params[param_len] = (sz32 & 0xff) as u8;
                param_len += 1;
            }
            if (sz32 & 0x0000_ff00) != 0 {
                cmd |= 0x20;
                params[param_len] = ((sz32 >> 8) & 0xff) as u8;
                param_len += 1;
            }
            if (sz32 & 0x00ff_0000) != 0 {
                cmd |= 0x40;
                params[param_len] = ((sz32 >> 16) & 0xff) as u8;
                param_len += 1;
            }
        }

        out.push(cmd);
        out.extend_from_slice(&params[..param_len]);

        offset += chunk_size;
        size -= chunk_size;
    }
}

fn read_size_varint(data: &[u8], pos: &mut usize) -> Result<usize, GirError> {
    let mut size: usize = 0;
    let mut shift: u32 = 0;
    loop {
        let b = read_byte(data, pos)?;
        size |= ((b & 0x7f) as usize) << shift;
        if (b & 0x80) == 0 {
            return Ok(size);
        }
        shift += 7;
        if shift >= usize::BITS {
            return Err(GirError::CorruptObject {
                id: "<delta>".to_string(),
                reason: "delta size varint overflow".to_string(),
            });
        }
    }
}

fn write_size_varint(out: &mut Vec<u8>, mut size: usize) {
    loop {
        let mut b = (size & 0x7f) as u8;
        size >>= 7;
        if size != 0 {
            b |= 0x80;
        }
        out.push(b);
        if size == 0 {
            break;
        }
    }
}

#[inline]
fn read_byte(data: &[u8], pos: &mut usize) -> Result<u8, GirError> {
    if *pos >= data.len() {
        return Err(GirError::CorruptObject {
            id: "<delta>".to_string(),
            reason: "unexpected EOF in delta stream".to_string(),
        });
    }
    let b = data[*pos];
    *pos += 1;
    Ok(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delta_compute_and_apply_roundtrip() {
        let mut base = Vec::new();
        for i in 0..500 {
            base.extend_from_slice(format!("line {i}: some shared rust source code content\n").as_bytes());
        }

        let mut target = base.clone();
        // Modify a section in the middle and append at the end
        target.splice(200..250, b"INSERTED NEW FUNCTION BODY HERE\n".iter().copied());
        target.extend_from_slice(b"// trailing comment added in v2\n");

        let delta = compute_delta(&base, &target).expect("should produce compact delta");
        assert!(delta.len() < target.len() / 5);

        let reconstructed = apply_delta(&base, &delta).unwrap();
        assert_eq!(reconstructed, target);
    }

    #[test]
    fn delta_large_64k_copy_roundtrip() {
        let base = vec![0x42u8; 70_000];
        let mut target = base.clone();
        target.extend_from_slice(b"extra tail");

        let delta = compute_delta(&base, &target).expect("should compress 70KB copy");
        let reconstructed = apply_delta(&base, &delta).unwrap();
        assert_eq!(reconstructed, target);
    }
}
