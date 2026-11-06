use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use gir_core::{GirError, Repository};

/// Parsed remote endpoint location supporting Smart HTTP/HTTPS, SSH, and Local paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteEndpoint {
    Http {
        url: String,
        host: String,
        port: u16,
        path: String,
        tls: bool,
    },
    Ssh {
        user_host: String,
        path: String,
    },
    Local {
        path: PathBuf,
    },
}

impl RemoteEndpoint {
    /// Parses a Git remote URL (`https://...`, `http://...`, `ssh://...`, `git@host:path`, `file://...`, or `/local/path`).
    pub fn parse(raw: &str) -> Result<Self, GirError> {
        let trimmed = raw.trim();
        if let Some(rest) = trimmed.strip_prefix("https://") {
            return parse_http_url(trimmed, rest, true);
        }
        if let Some(rest) = trimmed.strip_prefix("http://") {
            return parse_http_url(trimmed, rest, false);
        }
        if let Some(rest) = trimmed.strip_prefix("file://") {
            return Ok(Self::Local {
                path: PathBuf::from(rest),
            });
        }
        if let Some(rest) = trimmed.strip_prefix("ssh://") {
            let (user_host, path_part) = rest.split_once('/').ok_or_else(|| {
                GirError::InvalidArgument(format!("invalid ssh URL '{trimmed}'"))
            })?;
            return Ok(Self::Ssh {
                user_host: user_host.to_string(),
                path: format!("/{path_part}"),
            });
        }

        // Check SCP-like SSH syntax `user@host:path` (only when no `/` precedes `:`)
        if let Some((left, right)) = trimmed.split_once(':') {
            if !left.contains('/') && !left.is_empty() && !right.is_empty() {
                return Ok(Self::Ssh {
                    user_host: left.to_string(),
                    path: right.to_string(),
                });
            }
        }

        Ok(Self::Local {
            path: PathBuf::from(trimmed),
        })
    }
}

fn parse_http_url(full: &str, without_scheme: &str, tls: bool) -> Result<RemoteEndpoint, GirError> {
    let (host_port, path_part) = match without_scheme.split_once('/') {
        Some((h, p)) => (h, format!("/{}", p.trim_end_matches('/'))),
        None => (without_scheme, "/".to_string()),
    };
    let (host, port) = match host_port.split_once(':') {
        Some((h, port_str)) => {
            let p: u16 = port_str.parse().map_err(|_| {
                GirError::InvalidArgument(format!("invalid port in URL '{full}'"))
            })?;
            (h.to_string(), p)
        }
        None => (host_port.to_string(), if tls { 443 } else { 80 }),
    };

    let clean_url = full.trim_end_matches('/').to_string();
    Ok(RemoteEndpoint::Http {
        url: clean_url,
        host,
        port,
        path: path_part,
        tls,
    })
}

/// Basic-Auth username and password retrieved from Git's `credential.helper` protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

impl Credentials {
    /// Formats an `Authorization: Basic <base64>` HTTP header value.
    pub fn basic_auth_header(&self) -> String {
        let raw = format!("{}:{}", self.username, self.password);
        format!("Basic {}", base64_encode(raw.as_bytes()))
    }
}

/// Queries the configured `credential.helper` (from `.git/config` / `~/.gitconfig`) for `(protocol, host, path)`.
pub fn query_credential_helper(
    repo: Option<&Repository>,
    tls: bool,
    host: &str,
    path: &str,
) -> Option<Credentials> {
    let helper = resolve_credential_helper(repo)?;
    let protocol = if tls { "https" } else { "http" };
    let clean_path = path.trim_start_matches('/');
    let input = format!("protocol={protocol}\nhost={host}\npath={clean_path}\n\n");

    let cmd_line = if helper.starts_with('!') {
        format!("{} get", &helper[1..])
    } else if helper.starts_with('/') || helper.contains(' ') {
        format!("{helper} get")
    } else {
        format!("git credential-{helper} get")
    };

    let mut child = Command::new("sh")
        .arg("-c")
        .arg(&cmd_line)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    if let Some(ref mut stdin) = child.stdin {
        let _ = stdin.write_all(input.as_bytes());
    }
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&out.stdout);
    let mut username = None;
    let mut password = None;
    for line in text.lines() {
        if let Some(u) = line.strip_prefix("username=") {
            username = Some(u.to_string());
        } else if let Some(p) = line.strip_prefix("password=") {
            password = Some(p.to_string());
        }
    }

    Some(Credentials {
        username: username?,
        password: password?,
    })
}

fn resolve_credential_helper(repo: Option<&Repository>) -> Option<String> {
    if let Some(r) = repo {
        if let Ok(cfg) = r.config() {
            if let Some(h) = cfg.get("credential.helper") {
                if !h.trim().is_empty() {
                    return Some(h.trim().to_string());
                }
            }
        }
    }
    let global = gir_core::GitConfig::load_cascaded(None).ok()?;
    global
        .get("credential.helper")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}

/// Pure-Rust standard Base64 encoder (RFC 4648).
pub fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;

        out.push(TABLE[((triple >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((triple >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(triple & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}
