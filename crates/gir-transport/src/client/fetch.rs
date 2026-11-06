use std::collections::HashSet;
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::Path;

use gir_core::graph::peel_to_commit_object;
use gir_core::object::commit::Signature;
use gir_core::pack::unpack_pack_bytes;
use gir_core::storage::atomic::LockFile;
use gir_core::worktree::checkout_tree;
use gir_core::{GirError, ObjectId, Repository};

use crate::client::connect_transport;
use crate::pktline::{PktLine, PktLineReader, PktLineWriter};
use crate::refs_discovery::AdvertisedRefs;
use crate::sideband::SidebandChannel;

#[derive(Debug, Clone)]
pub struct UpdatedTrackingRef {
    pub remote_ref: String,
    pub local_tracking_ref: Option<String>,
    pub old_oid: ObjectId,
    pub new_oid: ObjectId,
}

#[derive(Debug, Clone)]
pub struct FetchOutcome {
    pub remote_url: String,
    pub advertised: AdvertisedRefs,
    pub updated_refs: Vec<UpdatedTrackingRef>,
    pub objects_received: bool,
}

/// Fetches objects and references from `remote_name_or_url` into `repo` (`gir fetch`).
pub fn fetch_from_remote(
    repo: &Repository,
    remote_name_or_url: &str,
    refspecs: &[String],
) -> Result<FetchOutcome, GirError> {
    let (remote_name, remote_url) = resolve_remote_name_and_url(repo, remote_name_or_url)?;
    let mut client = connect_transport(Some(repo), &remote_url)?;
    let advertised = client.discover_upload_pack()?;

    let target_refs = select_refs_to_fetch(&advertised, remote_name.as_deref(), refspecs);

    let mut wants: Vec<ObjectId> = Vec::new();
    let mut seen_wants: HashSet<ObjectId> = HashSet::new();
    for (oid, _, _) in &target_refs {
        if !repo.odb().exists(oid) && seen_wants.insert(*oid) {
            wants.push(*oid);
        }
    }

    let mut objects_received = false;
    if !wants.is_empty() {
        let use_sideband = advertised.has_capability("side-band-64k")
            || advertised.has_capability("side-band");

        let mut req_buf = Vec::new();
        {
            let mut w = PktLineWriter::new(&mut req_buf);
            let mut caps = vec!["ofs-delta", "agent=gir/0.1.0"];
            if use_sideband {
                caps.insert(0, "side-band-64k");
            }
            let caps_str = caps.join(" ");

            for (idx, oid) in wants.iter().enumerate() {
                if idx == 0 {
                    w.write_str(&format!("want {oid} {caps_str}\n"))?;
                } else {
                    w.write_str(&format!("want {oid}\n"))?;
                }
            }
            w.write_flush()?;

            let local_refs = repo.refs().list_all().unwrap_or_default();
            let mut sent_haves = HashSet::new();
            for (name, oid) in local_refs {
                if name.starts_with("refs/gir/") {
                    continue;
                }
                if sent_haves.insert(oid) {
                    w.write_str(&format!("have {oid}\n"))?;
                }
            }
            w.write_str("done\n")?;
        }

        let resp_bytes = client.upload_pack(&req_buf)?;
        let pack_bytes = extract_pack_from_upload_pack_response(&resp_bytes, use_sideband)?;
        if !pack_bytes.is_empty() {
            unpack_pack_bytes(&pack_bytes, repo.odb())?;
            let _ = repo.odb().refresh_packs();
            objects_received = true;
        }
    }

    let sig = repo
        .resolve_committer_signature()
        .unwrap_or_else(|_| Signature::now("gir", "gir@localhost"));

    let mut updated_refs = Vec::new();
    let mut fetch_head_lines = String::new();

    for (new_oid, remote_ref, tracking_ref_opt) in target_refs {
        let mut old_oid = ObjectId::ZERO;
        if let Some(ref tracking_ref) = tracking_ref_opt {
            old_oid = repo
                .refs()
                .resolve_to_oid(tracking_ref)
                .unwrap_or(ObjectId::ZERO);
            if old_oid != new_oid {
                repo.refs().update_ref(
                    tracking_ref,
                    new_oid,
                    None,
                    true,
                    &sig,
                    &format!("fetch {remote_name_or_url}: storing {remote_ref}"),
                )?;
            }
        }

        if let Some(branch_short) = remote_ref.strip_prefix("refs/heads/") {
            fetch_head_lines.push_str(&format!(
                "{new_oid}\t\tbranch '{branch_short}' of {remote_url}\n"
            ));
        } else {
            fetch_head_lines.push_str(&format!("{new_oid}\t\t{remote_ref} of {remote_url}\n"));
        }

        updated_refs.push(UpdatedTrackingRef {
            remote_ref,
            local_tracking_ref: tracking_ref_opt,
            old_oid,
            new_oid,
        });
    }

    if !fetch_head_lines.is_empty() {
        let fh_path = repo.git_dir().join("FETCH_HEAD");
        let mut lock = LockFile::acquire(&fh_path)?;
        lock.write_all(fetch_head_lines.as_bytes())?;
        lock.commit()?;
    }

    Ok(FetchOutcome {
        remote_url,
        advertised,
        updated_refs,
        objects_received,
    })
}

/// Clones a remote repository at `url` into `dest_dir` (`gir clone`).
pub fn clone_repository(
    url: &str,
    dest_dir: &Path,
    bare: bool,
) -> Result<Repository, GirError> {
    // Convert relative local source path to absolute before initializing `dest_dir`
    let effective_url = if !url.contains("://") && !url.contains('@') {
        let p = Path::new(url);
        if p.exists() {
            fs::canonicalize(p)
                .unwrap_or_else(|_| p.to_path_buf())
                .to_string_lossy()
                .into_owned()
        } else {
            url.to_string()
        }
    } else {
        url.to_string()
    };

    let repo = Repository::init(dest_dir, bare)?;
    {
        let mut cfg = repo.local_config()?;
        cfg.set("remote.origin.url", &effective_url)?;
        cfg.set(
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        )?;
        cfg.save_to(repo.git_dir().join("config"))?;
    }

    let outcome = fetch_from_remote(&repo, "origin", &[])?;

    // Determine default branch (`symref=HEAD:refs/heads/...` or matching `HEAD` OID)
    let default_branch_ref = if let Some(sym) = outcome.advertised.head_symref() {
        Some(sym.to_string())
    } else if let Some(head_oid) = outcome.advertised.find_ref("HEAD") {
        outcome
            .advertised
            .refs
            .iter()
            .find(|(oid, name)| *oid == head_oid && name.starts_with("refs/heads/"))
            .map(|(_, name)| name.clone())
    } else {
        outcome
            .advertised
            .refs
            .iter()
            .find(|(_, name)| name.starts_with("refs/heads/"))
            .map(|(_, name)| name.clone())
    };

    if let Some(branch_ref) = default_branch_ref {
        let branch_short = branch_ref
            .strip_prefix("refs/heads/")
            .unwrap_or(&branch_ref);

        if let Some(target_oid) = outcome.advertised.find_ref(&branch_ref) {
            let sig = repo
                .resolve_committer_signature()
                .unwrap_or_else(|_| Signature::now("gir", "gir@localhost"));
            let clone_msg = format!("clone: from {effective_url}");

            repo.refs()
                .update_ref(&branch_ref, target_oid, None, true, &sig, &clone_msg)?;
            repo.refs()
                .set_symbolic_ref("HEAD", &branch_ref, Some(&sig), Some(&clone_msg))?;

            if !bare {
                let _ = repo.refs().set_symbolic_ref(
                    "refs/remotes/origin/HEAD",
                    &format!("refs/remotes/origin/{branch_short}"),
                    None,
                    None,
                );

                let mut cfg = repo.local_config()?;
                cfg.set(&format!("branch.{branch_short}.remote"), "origin")?;
                cfg.set(&format!("branch.{branch_short}.merge"), &branch_ref)?;
                cfg.save_to(repo.git_dir().join("config"))?;

                let (_, commit) = peel_to_commit_object(repo.odb(), &target_oid)?;
                checkout_tree(&repo, &commit.tree, true, "clone")?;
            }
        }
    }

    Ok(repo)
}

pub fn resolve_remote_name_and_url(
    repo: &Repository,
    remote_name_or_url: &str,
) -> Result<(Option<String>, String), GirError> {
    let cfg = repo.config()?;
    let key = format!("remote.{remote_name_or_url}.url");
    if let Some(url) = cfg.get(&key) {
        return Ok((Some(remote_name_or_url.to_string()), url.to_string()));
    }
    Ok((None, remote_name_or_url.to_string()))
}

fn select_refs_to_fetch(
    advertised: &AdvertisedRefs,
    remote_name: Option<&str>,
    refspecs: &[String],
) -> Vec<(ObjectId, String, Option<String>)> {
    let mut out = Vec::new();

    if refspecs.is_empty() {
        for (oid, name) in &advertised.refs {
            if let Some(branch) = name.strip_prefix("refs/heads/") {
                let tracking = remote_name.map(|r| format!("refs/remotes/{r}/{branch}"));
                out.push((*oid, name.clone(), tracking));
            } else if name.starts_with("refs/tags/") && !name.ends_with("^{}") {
                out.push((*oid, name.clone(), Some(name.clone())));
            }
        }
        return out;
    }

    for spec in refspecs {
        let clean = spec.strip_prefix('+').unwrap_or(spec);
        let (src, explicit_dst) = match clean.split_once(':') {
            Some((s, d)) => (s, Some(d.to_string())),
            None => (clean, None),
        };

        let candidates = [
            src.to_string(),
            format!("refs/heads/{src}"),
            format!("refs/tags/{src}"),
        ];
        for (oid, adv_name) in &advertised.refs {
            if candidates.iter().any(|c| c == adv_name) {
                let tracking = explicit_dst.clone().or_else(|| {
                    adv_name
                        .strip_prefix("refs/heads/")
                        .and_then(|b| remote_name.map(|r| format!("refs/remotes/{r}/{b}")))
                });
                out.push((*oid, adv_name.clone(), tracking));
                break;
            }
        }
    }

    out
}

fn extract_pack_from_upload_pack_response(
    resp_bytes: &[u8],
    use_sideband: bool,
) -> Result<Vec<u8>, GirError> {
    if !use_sideband {
        // Locate `PACK` magic signature after `ACK`/`NAK` pkt-lines
        if let Some(pos) = resp_bytes.windows(4).position(|w| w == b"PACK") {
            return Ok(resp_bytes[pos..].to_vec());
        }
        return Ok(Vec::new());
    }

    let mut reader = PktLineReader::new(Cursor::new(resp_bytes));
    let mut pack_data = Vec::new();

    while let Some(pkt) = reader.read_pkt()? {
        match pkt {
            PktLine::Flush | PktLine::ResponseEnd => break,
            PktLine::Delimiter => continue,
            PktLine::Data(bytes) => {
                if bytes.is_empty() {
                    continue;
                }
                if bytes.starts_with(b"ACK ") || bytes == b"NAK\n" || bytes == b"NAK" {
                    continue;
                }
                match SidebandChannel::from_byte(bytes[0]) {
                    Ok(SidebandChannel::PackData) => {
                        pack_data.extend_from_slice(&bytes[1..]);
                    }
                    Ok(SidebandChannel::Progress) => {}
                    Ok(SidebandChannel::FatalError) => {
                        let err = String::from_utf8_lossy(&bytes[1..]).trim().to_string();
                        return Err(GirError::InvalidArgument(format!("remote error: {err}")));
                    }
                    Err(_) => {
                        if bytes.starts_with(b"PACK") {
                            pack_data.extend_from_slice(&bytes);
                            let _ = reader.inner_mut().read_to_end(&mut pack_data);
                            break;
                        }
                    }
                }
            }
        }
    }

    Ok(pack_data)
}
