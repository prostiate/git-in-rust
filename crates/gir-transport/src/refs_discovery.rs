use std::collections::BTreeMap;
use std::io::Read;

use gir_core::{GirError, ObjectId};

use crate::pktline::{PktLine, PktLineReader, PktLineWriter};

/// Parsed reference advertisement and server capabilities from `git-upload-pack` or `git-receive-pack`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdvertisedRefs {
    /// Advertised `(oid, ref_name)` pairs in wire order (starting with `HEAD` if present).
    pub refs: Vec<(ObjectId, String)>,
    /// Server capabilities advertised after the NUL byte on the first ref line.
    pub capabilities: Vec<String>,
    /// Symbolic ref mappings parsed from `symref=<from>:<to>` capabilities (e.g. `HEAD -> refs/heads/main`).
    pub symrefs: BTreeMap<String, String>,
}

impl AdvertisedRefs {
    pub fn has_capability(&self, cap: &str) -> bool {
        self.capabilities
            .iter()
            .any(|c| c == cap || c.starts_with(&format!("{cap}=")))
    }

    pub fn head_symref(&self) -> Option<&str> {
        self.symrefs.get("HEAD").map(String::as_str)
    }

    pub fn find_ref(&self, name: &str) -> Option<ObjectId> {
        self.refs
            .iter()
            .find(|(_, r)| r == name)
            .map(|(oid, _)| *oid)
    }
}

/// Parses an advertised-refs stream (both Smart HTTP with `# service=...` preamble and direct SSH/local).
pub fn parse_advertised_refs<R: Read>(
    reader: &mut PktLineReader<R>,
    expected_service: Option<&str>,
) -> Result<AdvertisedRefs, GirError> {
    let mut adv = AdvertisedRefs::default();
    let mut is_first_ref = true;

    while let Some(pkt) = reader.read_pkt()? {
        match pkt {
            PktLine::Flush => {
                // If we only just consumed the `# service=git-...` header packet, continue reading the actual ref list!
                if is_first_ref {
                    continue;
                }
                break;
            }
            PktLine::Delimiter | PktLine::ResponseEnd => continue,
            PktLine::Data(bytes) => {
                let text = String::from_utf8_lossy(&bytes);
                let trimmed = text.trim_end_matches(&['\r', '\n'][..]);

                if trimmed.starts_with("# service=") {
                    if let Some(exp) = expected_service {
                        let expected_line = format!("# service={exp}");
                        if trimmed != expected_line {
                            return Err(GirError::InvalidArgument(format!(
                                "unexpected remote service header: expected '{expected_line}', got '{trimmed}'"
                            )));
                        }
                    }
                    continue;
                }

                let (ref_part, caps_part) = if is_first_ref {
                    is_first_ref = false;
                    match trimmed.split_once('\0') {
                        Some((r, c)) => (r, Some(c)),
                        None => (trimmed, None),
                    }
                } else {
                    (trimmed, None)
                };

                if let Some(caps_str) = caps_part {
                    for cap in caps_str.split_whitespace().filter(|s| !s.is_empty()) {
                        if let Some(rest) = cap.strip_prefix("symref=") {
                            if let Some((from, to)) = rest.split_once(':') {
                                adv.symrefs.insert(from.to_string(), to.to_string());
                            }
                        }
                        adv.capabilities.push(cap.to_string());
                    }
                }

                let Some((sha_hex, ref_name)) = ref_part.split_once(' ') else {
                    continue;
                };
                if ref_name == "capabilities^{}" {
                    // Empty repository advertisement (`0000000000000000000000000000000000000000 capabilities^{}\0...`)
                    continue;
                }
                let oid = ObjectId::from_hex(sha_hex)?;
                adv.refs.push((oid, ref_name.to_string()));
            }
        }
    }

    Ok(adv)
}

/// Formats an advertised-refs payload for `git-upload-pack` or `git-receive-pack`.
pub fn format_advertised_refs(
    refs: &[(ObjectId, String)],
    capabilities: &[String],
    service_header: Option<&str>,
) -> Result<Vec<u8>, GirError> {
    let mut out = Vec::new();
    let mut writer = PktLineWriter::new(&mut out);

    if let Some(svc) = service_header {
        writer.write_str(&format!("# service={svc}\n"))?;
        writer.write_flush()?;
    }

    let caps_joined = capabilities.join(" ");
    if refs.is_empty() {
        writer.write_str(&format!(
            "{} capabilities^{{}}\0{caps_joined}\n",
            ObjectId::ZERO
        ))?;
    } else {
        for (idx, (oid, name)) in refs.iter().enumerate() {
            if idx == 0 {
                writer.write_str(&format!("{oid} {name}\0{caps_joined}\n"))?;
            } else {
                writer.write_str(&format!("{oid} {name}\n"))?;
            }
        }
    }

    writer.write_flush()?;
    Ok(out)
}
