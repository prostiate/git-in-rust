use std::collections::{HashSet, VecDeque};
use std::io::Cursor;

use gir_core::object::ParsedObject;
use gir_core::pack::build_pack_bytes;
use gir_core::refs::RefTarget;
use gir_core::{GirError, ObjectDatabase, ObjectId, Repository};

use crate::pktline::{PktLine, PktLineReader, PktLineWriter};
use crate::refs_discovery::format_advertised_refs;
use crate::sideband::{write_sideband_frames, SidebandChannel};

/// Builds the `git-upload-pack` reference advertisement payload for `repo`.
pub fn advertise_upload_pack_refs(
    repo: &Repository,
    include_service_header: bool,
) -> Result<Vec<u8>, GirError> {
    let mut refs = Vec::new();
    let mut caps = vec![
        "multi_ack".to_string(),
        "multi_ack_detailed".to_string(),
        "side-band-64k".to_string(),
        "ofs-delta".to_string(),
        "agent=gir/0.1.0".to_string(),
    ];

    if let Ok(Some(RefTarget::Symbolic(sym))) = repo.refs().read_raw("HEAD") {
        caps.insert(0, format!("symref=HEAD:{sym}"));
    }

    if let Ok(head_oid) = repo.refs().resolve_to_oid("HEAD") {
        refs.push((head_oid, "HEAD".to_string()));
    }

    for (name, oid) in repo.refs().list_all()? {
        if name == "HEAD" || name.starts_with("refs/gir/") {
            continue;
        }
        refs.push((oid, name));
    }

    let svc = if include_service_header {
        Some("git-upload-pack")
    } else {
        None
    };
    format_advertised_refs(&refs, &caps, svc)
}

/// Processes a client `git-upload-pack` request (`want` / `have` / `done`) against `repo`
/// and returns the `ACK`/`NAK` + packfile response stream.
pub fn handle_upload_pack(repo: &Repository, request_bytes: &[u8]) -> Result<Vec<u8>, GirError> {
    let mut reader = PktLineReader::new(Cursor::new(request_bytes));
    let mut wants: Vec<ObjectId> = Vec::new();
    let mut haves: Vec<ObjectId> = Vec::new();
    let mut client_caps: HashSet<String> = HashSet::new();

    while let Some(pkt) = reader.read_pkt()? {
        match pkt {
            PktLine::Flush | PktLine::Delimiter => continue,
            PktLine::ResponseEnd => break,
            PktLine::Data(bytes) => {
                let line = String::from_utf8_lossy(&bytes);
                let trimmed = line.trim_end_matches(&['\r', '\n'][..]);
                if let Some(rest) = trimmed.strip_prefix("want ") {
                    let normalized = rest.replace('\0', " ");
                    let mut parts = normalized.split_whitespace();
                    if let Some(sha_str) = parts.next() {
                        wants.push(ObjectId::from_hex(sha_str)?);
                    }
                    for cap in parts {
                        client_caps.insert(cap.to_string());
                    }
                } else if let Some(rest) = trimmed.strip_prefix("have ") {
                    let sha_str = rest.trim();
                    if let Ok(oid) = ObjectId::from_hex(sha_str) {
                        haves.push(oid);
                    }
                } else if trimmed == "done" {
                    break;
                }
            }
        }
    }

    let mut out = Vec::new();
    let mut writer = PktLineWriter::new(&mut out);

    if wants.is_empty() {
        writer.write_flush()?;
        return Ok(out);
    }

    let odb = repo.odb();
    let mut common_haves = Vec::new();
    for h in &haves {
        if odb.exists(h) {
            common_haves.push(*h);
        }
    }

    if let Some(first_common) = common_haves.first() {
        writer.write_str(&format!("ACK {first_common}\n"))?;
    } else {
        writer.write_str("NAK\n")?;
    }

    let excluded_objects = collect_reachable_objects(odb, &common_haves)?;
    let wanted_oids = collect_reachable_objects_excluding(odb, &wants, &excluded_objects)?;
    let mut wanted_objects = Vec::with_capacity(wanted_oids.len());
    for oid in &wanted_oids {
        wanted_objects.push((*oid, odb.read_raw(oid)?));
    }
    let (pack_bytes, _, _) = build_pack_bytes(&wanted_objects)?;

    let use_sideband =
        client_caps.contains("side-band-64k") || client_caps.contains("side-band");
    if use_sideband {
        let msg = format!("Counting objects: {}, done.\n", wanted_objects.len());
        write_sideband_frames(&mut writer, SidebandChannel::Progress, msg.as_bytes())?;
        write_sideband_frames(&mut writer, SidebandChannel::PackData, &pack_bytes)?;
        writer.write_flush()?;
    } else {
        writer.into_inner().extend_from_slice(&pack_bytes);
    }

    Ok(out)
}

/// Collects all reachable objects (commits, trees, blobs, tags) starting from `roots`.
pub fn collect_reachable_objects(
    odb: &ObjectDatabase,
    roots: &[ObjectId],
) -> Result<HashSet<ObjectId>, GirError> {
    let empty = HashSet::new();
    let list = collect_reachable_objects_excluding(odb, roots, &empty)?;
    Ok(list.into_iter().collect())
}

/// Collects all objects reachable from `roots` that are not in `excluded`, preserving traversal order.
pub fn collect_reachable_objects_excluding(
    odb: &ObjectDatabase,
    roots: &[ObjectId],
    excluded: &HashSet<ObjectId>,
) -> Result<Vec<ObjectId>, GirError> {
    let mut seen: HashSet<ObjectId> = excluded.clone();
    let mut ordered = Vec::new();
    let mut queue = VecDeque::new();

    for &root in roots {
        if seen.insert(root) {
            queue.push_back(root);
        }
    }

    while let Some(oid) = queue.pop_front() {
        let Ok(parsed) = odb.read_parsed(&oid) else {
            continue;
        };
        ordered.push(oid);

        match parsed {
            ParsedObject::Commit(c) => {
                if seen.insert(c.tree) {
                    queue.push_back(c.tree);
                }
                for p in c.parents {
                    if seen.insert(p) {
                        queue.push_back(p);
                    }
                }
            }
            ParsedObject::Tree(t) => {
                for entry in t.entries {
                    if seen.insert(entry.oid) {
                        queue.push_back(entry.oid);
                    }
                }
            }
            ParsedObject::Tag(tag) => {
                if seen.insert(tag.object) {
                    queue.push_back(tag.object);
                }
            }
            ParsedObject::Blob(_) => {}
        }
    }

    Ok(ordered)
}
