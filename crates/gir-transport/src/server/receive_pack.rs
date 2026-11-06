use std::collections::HashSet;
use std::io::{Cursor, Read};

use gir_core::object::commit::Signature;
use gir_core::pack::unpack_pack_bytes;
use gir_core::{GirError, ObjectId, Repository};

use crate::pktline::{PktLine, PktLineReader, PktLineWriter};
use crate::refs_discovery::format_advertised_refs;
use crate::sideband::{write_sideband_frames, SidebandChannel};

#[derive(Debug, Clone)]
struct PushCommand {
    old_oid: ObjectId,
    new_oid: ObjectId,
    ref_name: String,
}

/// Builds the `git-receive-pack` reference advertisement payload for `repo`.
pub fn advertise_receive_pack_refs(
    repo: &Repository,
    include_service_header: bool,
) -> Result<Vec<u8>, GirError> {
    let mut refs = Vec::new();
    for (name, oid) in repo.refs().list_all()? {
        if name == "HEAD" || name.starts_with("refs/gir/") {
            continue;
        }
        refs.push((oid, name));
    }

    let caps = vec![
        "report-status".to_string(),
        "delete-refs".to_string(),
        "side-band-64k".to_string(),
        "ofs-delta".to_string(),
        "agent=gir/0.1.0".to_string(),
    ];

    let svc = if include_service_header {
        Some("git-receive-pack")
    } else {
        None
    };
    format_advertised_refs(&refs, &caps, svc)
}

/// Processes a client `git-receive-pack` request (ref-update commands + trailing `.pack` stream)
/// against `repo` and returns the `report-status` response.
pub fn handle_receive_pack(repo: &Repository, request_bytes: &[u8]) -> Result<Vec<u8>, GirError> {
    let mut reader = PktLineReader::new(Cursor::new(request_bytes));
    let mut commands = Vec::new();
    let mut client_caps: HashSet<String> = HashSet::new();

    while let Some(pkt) = reader.read_pkt()? {
        match pkt {
            PktLine::Flush => break,
            PktLine::Delimiter | PktLine::ResponseEnd => continue,
            PktLine::Data(bytes) => {
                let text = String::from_utf8_lossy(&bytes);
                let trimmed = text.trim_end_matches(&['\r', '\n'][..]);
                let (cmd_part, caps_opt) = match trimmed.split_once('\0') {
                    Some((c, caps)) => (c, Some(caps)),
                    None => (trimmed, None),
                };

                if let Some(caps_str) = caps_opt {
                    for cap in caps_str.split_whitespace().filter(|s| !s.is_empty()) {
                        client_caps.insert(cap.to_string());
                    }
                }

                let mut parts = cmd_part.split_whitespace();
                let (Some(old_hex), Some(new_hex), Some(ref_name)) =
                    (parts.next(), parts.next(), parts.next())
                else {
                    continue;
                };
                commands.push(PushCommand {
                    old_oid: ObjectId::from_hex(old_hex)?,
                    new_oid: ObjectId::from_hex(new_hex)?,
                    ref_name: ref_name.to_string(),
                });
            }
        }
    }

    // Read remaining packfile bytes after `0000`
    let mut pack_bytes = Vec::new();
    reader.inner_mut().read_to_end(&mut pack_bytes)?;

    let unpack_status = if !pack_bytes.is_empty() {
        match unpack_pack_bytes(&pack_bytes, repo.odb()) {
            Ok(_) => "unpack ok\n".to_string(),
            Err(e) => format!("unpack {e}\n"),
        }
    } else {
        "unpack ok\n".to_string()
    };

    let sig = repo
        .resolve_committer_signature()
        .unwrap_or_else(|_| Signature::now("gir-receive-pack", "gir@localhost"));

    let mut ref_statuses = Vec::new();
    let unpack_ok = unpack_status == "unpack ok\n";

    for cmd in &commands {
        if !unpack_ok {
            ref_statuses.push(format!("ng {} unpacker error\n", cmd.ref_name));
            continue;
        }
        if cmd.new_oid == ObjectId::ZERO {
            match repo.refs().delete_ref(&cmd.ref_name, Some(cmd.old_oid)) {
                Ok(()) => ref_statuses.push(format!("ok {}\n", cmd.ref_name)),
                Err(e) => ref_statuses.push(format!("ng {} {e}\n", cmd.ref_name)),
            }
        } else {
            let res = repo.refs().update_ref(
                &cmd.ref_name,
                cmd.new_oid,
                Some(cmd.old_oid),
                true,
                &sig,
                "push",
            );
            match res {
                Ok(_) => ref_statuses.push(format!("ok {}\n", cmd.ref_name)),
                Err(e) => ref_statuses.push(format!("ng {} {e}\n", cmd.ref_name)),
            }
        }
    }

    // If `HEAD` in the target repository is currently unborn (points to a branch that was just created),
    // or if `refs/heads/main` was pushed to an empty repo whose `HEAD` points to `refs/heads/master`,
    // ensure `HEAD` points to a valid pushed branch if its current target doesn't exist.
    if repo.refs().resolve_to_oid("HEAD").is_err() {
        if let Some(first_created) = commands
            .iter()
            .find(|c| c.new_oid != ObjectId::ZERO && c.ref_name.starts_with("refs/heads/"))
        {
            let _ = repo
                .refs()
                .set_symbolic_ref("HEAD", &first_created.ref_name, None, None);
        }
    }

    if !client_caps.contains("report-status") {
        return Ok(Vec::new());
    }

    let mut status_pkt_stream = Vec::new();
    {
        let mut sw = PktLineWriter::new(&mut status_pkt_stream);
        sw.write_str(&unpack_status)?;
        for s in &ref_statuses {
            sw.write_str(s)?;
        }
        sw.write_flush()?;
    }

    if client_caps.contains("side-band-64k") || client_caps.contains("side-band") {
        let mut muxed = Vec::new();
        let mut mw = PktLineWriter::new(&mut muxed);
        write_sideband_frames(&mut mw, SidebandChannel::PackData, &status_pkt_stream)?;
        mw.write_flush()?;
        Ok(muxed)
    } else {
        Ok(status_pkt_stream)
    }
}
