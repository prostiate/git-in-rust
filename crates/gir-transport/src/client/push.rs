use std::collections::HashSet;
use std::io::Cursor;

use gir_core::graph::is_ancestor;
use gir_core::hooks::run_hook_with_stdin;
use gir_core::object::commit::Signature;
use gir_core::pack::build_pack_bytes;
use gir_core::refs::RefTarget;
use gir_core::revision::parse_revision;
use gir_core::{GirError, ObjectId, Repository};

use crate::client::fetch::resolve_remote_name_and_url;
use crate::client::connect_transport;
use crate::pktline::{PktLine, PktLineReader, PktLineWriter};
use crate::server::{collect_reachable_objects, collect_reachable_objects_excluding};
use crate::sideband::demux_sideband_stream;

#[derive(Debug, Clone, Default)]
pub struct PushOptions {
    pub remote: Option<String>,
    pub refspecs: Vec<String>,
    pub set_upstream: bool,
    pub force: bool,
    pub no_verify: bool,
}

#[derive(Debug, Clone)]
pub struct PushedRefUpdate {
    pub local_branch: Option<String>,
    pub remote_ref: String,
    pub old_oid: ObjectId,
    pub new_oid: ObjectId,
}

#[derive(Debug, Clone)]
pub struct PushOutcome {
    pub remote_name: Option<String>,
    pub remote_url: String,
    pub updates: Vec<PushedRefUpdate>,
    pub up_to_date: bool,
}

/// Pushes local commits and references to a remote repository (`gir push`).
pub fn push_to_remote(repo: &Repository, opts: &PushOptions) -> Result<PushOutcome, GirError> {
    let default_remote = resolve_default_push_remote(repo);
    let remote_arg = opts.remote.as_deref().unwrap_or(&default_remote);
    let (remote_name, remote_url) = resolve_remote_name_and_url(repo, remote_arg)?;

    let mut client = connect_transport(Some(repo), &remote_url)?;
    let advertised = client.discover_receive_pack()?;

    let updates = resolve_push_updates(
        repo,
        &advertised,
        remote_name.as_deref(),
        &opts.refspecs,
        opts.force,
    )?;

    let active_updates: Vec<PushedRefUpdate> = updates
        .into_iter()
        .filter(|u| u.old_oid != u.new_oid)
        .collect();

    if active_updates.is_empty() {
        // Even if everything is up-to-date, apply `-u` / `--set-upstream` if requested
        if opts.set_upstream {
            configure_upstream_for_current_branch(repo, remote_name.as_deref().unwrap_or(remote_arg))?;
        }
        return Ok(PushOutcome {
            remote_name,
            remote_url,
            updates: Vec::new(),
            up_to_date: true,
        });
    }

    // Execute `pre-push` hook unless `--no-verify`
    if !opts.no_verify {
        let mut hook_stdin = String::new();
        for u in &active_updates {
            let local_ref = u
                .local_branch
                .as_deref()
                .map(|b| format!("refs/heads/{b}"))
                .unwrap_or_else(|| u.remote_ref.clone());
            hook_stdin.push_str(&format!(
                "{local_ref} {} {} {}\n",
                u.new_oid, u.remote_ref, u.old_oid
            ));
        }
        run_hook_with_stdin(
            repo,
            "pre-push",
            &[remote_arg, &remote_url],
            hook_stdin.as_bytes(),
        )?;
    }

    // Collect objects reachable from `new_oid`s excluding objects reachable from advertised remote refs
    let remote_known_roots: Vec<ObjectId> = advertised
        .refs
        .iter()
        .map(|(oid, _)| *oid)
        .filter(|oid| repo.odb().exists(oid))
        .collect();
    let excluded: HashSet<ObjectId> = collect_reachable_objects(repo.odb(), &remote_known_roots)?;

    let push_roots: Vec<ObjectId> = active_updates
        .iter()
        .map(|u| u.new_oid)
        .filter(|oid| *oid != ObjectId::ZERO)
        .collect();
    let has_non_delete_update = !push_roots.is_empty();
    let pack_bytes = if has_non_delete_update {
        let objects_to_send =
            collect_reachable_objects_excluding(repo.odb(), &push_roots, &excluded)?;
        let mut raw_objects = Vec::with_capacity(objects_to_send.len());
        for oid in &objects_to_send {
            raw_objects.push((*oid, repo.odb().read_raw(oid)?));
        }
        let (bytes, _, _) = build_pack_bytes(&raw_objects)?;
        bytes
    } else {
        Vec::new()
    };

    let use_sideband = advertised.has_capability("side-band-64k")
        || advertised.has_capability("side-band");

    let mut req_bytes = Vec::new();
    {
        let mut w = PktLineWriter::new(&mut req_bytes);
        let mut caps = vec!["report-status", "ofs-delta", "agent=gir/0.1.0"];
        if use_sideband {
            caps.push("side-band-64k");
        }
        let caps_str = caps.join(" ");

        for (idx, u) in active_updates.iter().enumerate() {
            if idx == 0 {
                w.write_str(&format!(
                    "{} {} {}\0 {caps_str}\n",
                    u.old_oid, u.new_oid, u.remote_ref
                ))?;
            } else {
                w.write_str(&format!("{} {} {}\n", u.old_oid, u.new_oid, u.remote_ref))?;
            }
        }
        w.write_flush()?;
    }
    if has_non_delete_update {
        req_bytes.extend_from_slice(&pack_bytes);
    }

    let resp_bytes = client.receive_pack(&req_bytes)?;
    verify_receive_pack_status(&resp_bytes, use_sideband)?;

    // Update local remote-tracking branches (`refs/remotes/<remote>/<branch>`) and `-u` config
    let sig = repo
        .resolve_committer_signature()
        .unwrap_or_else(|_| Signature::now("gir", "gir@localhost"));

    if let Some(ref r_name) = remote_name {
        for u in &active_updates {
            if let Some(branch_short) = u.remote_ref.strip_prefix("refs/heads/") {
                let tracking_ref = format!("refs/remotes/{r_name}/{branch_short}");
                if u.new_oid == ObjectId::ZERO {
                    let _ = repo.refs().delete_ref(&tracking_ref, None);
                } else {
                    repo.refs().update_ref(
                        &tracking_ref,
                        u.new_oid,
                        None,
                        true,
                        &sig,
                        "update by push",
                    )?;
                }
            }
        }
    }

    if opts.set_upstream {
        let r_label = remote_name.as_deref().unwrap_or(remote_arg);
        for u in &active_updates {
            if let Some(ref local_b) = u.local_branch {
                let mut cfg = repo.local_config()?;
                cfg.set(&format!("branch.{local_b}.remote"), r_label)?;
                cfg.set(&format!("branch.{local_b}.merge"), &u.remote_ref)?;
                cfg.save_to(repo.git_dir().join("config"))?;
            }
        }
    }

    Ok(PushOutcome {
        remote_name,
        remote_url,
        updates: active_updates,
        up_to_date: false,
    })
}

fn configure_upstream_for_current_branch(
    repo: &Repository,
    remote_name: &str,
) -> Result<(), GirError> {
    if let Ok(Some(RefTarget::Symbolic(sym))) = repo.refs().read_raw("HEAD") {
        if let Some(short) = sym.strip_prefix("refs/heads/") {
            let mut cfg = repo.local_config()?;
            cfg.set(&format!("branch.{short}.remote"), remote_name)?;
            cfg.set(&format!("branch.{short}.merge"), &sym)?;
            cfg.save_to(repo.git_dir().join("config"))?;
        }
    }
    Ok(())
}

fn resolve_default_push_remote(repo: &Repository) -> String {
    if let Ok(Some(RefTarget::Symbolic(sym))) = repo.refs().read_raw("HEAD") {
        if let Some(short) = sym.strip_prefix("refs/heads/") {
            if let Ok(cfg) = repo.config() {
                if let Some(r) = cfg.get(&format!("branch.{short}.remote")) {
                    if !r.trim().is_empty() {
                        return r.trim().to_string();
                    }
                }
            }
        }
    }
    "origin".to_string()
}

fn resolve_push_updates(
    repo: &Repository,
    advertised: &crate::refs_discovery::AdvertisedRefs,
    remote_name: Option<&str>,
    refspecs: &[String],
    force: bool,
) -> Result<Vec<PushedRefUpdate>, GirError> {
    let mut updates = Vec::new();

    let effective_specs: Vec<String> = if refspecs.is_empty() {
        match repo.refs().read_raw("HEAD")? {
            Some(RefTarget::Symbolic(sym)) => {
                let short = sym.strip_prefix("refs/heads/").unwrap_or(&sym);
                let mut spec = short.to_string();
                if let Ok(cfg) = repo.config() {
                    let branch_remote = cfg.get(&format!("branch.{short}.remote"));
                    if branch_remote == remote_name {
                        if let Some(merge_ref) = cfg.get(&format!("branch.{short}.merge")) {
                            if !merge_ref.trim().is_empty() {
                                spec = format!("{short}:{}", merge_ref.trim());
                            }
                        }
                    }
                }
                vec![spec]
            }
            _ => {
                return Err(GirError::InvalidArgument(
                    "You are not currently on a branch.\nTo push the history leading to the current (detached HEAD) state now, use\n\n    gir push origin HEAD:<name-of-remote-branch>".to_string(),
                ));
            }
        }
    } else {
        refspecs.to_vec()
    };

    for raw_spec in effective_specs {
        let is_forced = force || raw_spec.starts_with('+');
        let clean = raw_spec.strip_prefix('+').unwrap_or(&raw_spec);
        let (src, dst_opt) = match clean.split_once(':') {
            Some((s, d)) => (s, Some(d)),
            None => (clean, None),
        };

        if src.is_empty() {
            // Delete remote ref (`:branch`)
            let Some(dst) = dst_opt else {
                continue;
            };
            let remote_ref = normalize_remote_ref_name(dst);
            let old_oid = advertised.find_ref(&remote_ref).unwrap_or(ObjectId::ZERO);
            updates.push(PushedRefUpdate {
                local_branch: None,
                remote_ref,
                old_oid,
                new_oid: ObjectId::ZERO,
            });
            continue;
        }

        let new_oid = parse_revision(repo, src)?;
        let local_branch = if repo
            .refs()
            .resolve_to_oid(&format!("refs/heads/{src}"))
            .is_ok()
        {
            Some(src.to_string())
        } else {
            None
        };

        let remote_ref = match dst_opt {
            Some(d) => normalize_remote_ref_name(d),
            None => {
                if src.starts_with("refs/") {
                    src.to_string()
                } else {
                    format!("refs/heads/{src}")
                }
            }
        };

        let old_oid = advertised.find_ref(&remote_ref).unwrap_or(ObjectId::ZERO);

        if !is_forced && old_oid != ObjectId::ZERO && old_oid != new_oid {
            let is_ff = repo.odb().exists(&old_oid)
                && is_ancestor(repo.odb(), &old_oid, &new_oid).unwrap_or(false);
            if !is_ff {
                return Err(GirError::InvalidArgument(format!(
                    "Updates were rejected because the remote contains work that you do not have locally for '{remote_ref}' (non-fast-forward)."
                )));
            }
        }

        updates.push(PushedRefUpdate {
            local_branch,
            remote_ref,
            old_oid,
            new_oid,
        });
    }

    Ok(updates)
}

fn normalize_remote_ref_name(raw: &str) -> String {
    if raw.starts_with("refs/") {
        raw.to_string()
    } else {
        format!("refs/heads/{raw}")
    }
}

fn verify_receive_pack_status(resp_bytes: &[u8], use_sideband: bool) -> Result<(), GirError> {
    if resp_bytes.is_empty() {
        return Ok(());
    }

    let status_stream = if use_sideband {
        let mut r = PktLineReader::new(Cursor::new(resp_bytes));
        demux_sideband_stream(&mut r, |_| {})?
    } else {
        resp_bytes.to_vec()
    };

    let mut reader = PktLineReader::new(Cursor::new(status_stream));
    while let Some(pkt) = reader.read_pkt()? {
        match pkt {
            PktLine::Flush | PktLine::ResponseEnd => break,
            PktLine::Delimiter => continue,
            PktLine::Data(bytes) => {
                let line = String::from_utf8_lossy(&bytes);
                let trimmed = line.trim_end_matches(&['\r', '\n'][..]);
                if let Some(rest) = trimmed.strip_prefix("unpack ") {
                    if rest != "ok" {
                        return Err(GirError::InvalidArgument(format!(
                            "remote unpack failed: {rest}"
                        )));
                    }
                } else if let Some(rest) = trimmed.strip_prefix("ng ") {
                    return Err(GirError::InvalidArgument(format!(
                        "remote rejected ref update: {rest}"
                    )));
                }
            }
        }
    }

    Ok(())
}
