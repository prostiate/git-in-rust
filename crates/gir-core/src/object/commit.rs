use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::GirError;
use crate::hash::ObjectId;
use crate::object::{ObjectKind, RawObject};

/// A Git identity and timestamp (`Name <email> 1730000000 +0700`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    pub name: String,
    pub email: String,
    pub timestamp: i64,
    pub tz_offset: String,
}

impl Signature {
    /// Creates a `Signature` with the specified name, email, unix timestamp, and timezone (`"+0700"`).
    pub fn new(
        name: impl Into<String>,
        email: impl Into<String>,
        timestamp: i64,
        tz_offset: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            email: email.into(),
            timestamp,
            tz_offset: tz_offset.into(),
        }
    }

    /// Creates a `Signature` using the current system clock and the OS local timezone offset (e.g. `"+0700"`).
    pub fn now(name: impl Into<String>, email: impl Into<String>) -> Self {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let tz = local_tz_offset_string(secs);
        Self::new(name, email, secs, tz)
    }

    /// Parses a Git date override string such as `"1730000000 +0700"` or `"@1730000000 +0700"`.
    pub fn parse_git_date(date_str: &str) -> Option<(i64, String)> {
        let trimmed = date_str.trim();
        let (ts_part, tz_part) = trimmed.split_once(' ')?;
        let ts_clean = ts_part.strip_prefix('@').unwrap_or(ts_part);
        let ts: i64 = ts_clean.parse().ok()?;
        Some((ts, tz_part.trim().to_string()))
    }

    /// Parses a raw Git header signature line (`"Name <email> 1730000000 +0700"`).
    pub fn parse(id_hint: &str, raw: &str) -> Result<Self, GirError> {
        let lt_pos = raw.find('<').ok_or_else(|| GirError::CorruptObject {
            id: id_hint.to_string(),
            reason: format!("missing '<' in signature '{raw}'"),
        })?;
        let gt_pos = raw.rfind('>').ok_or_else(|| GirError::CorruptObject {
            id: id_hint.to_string(),
            reason: format!("missing '>' in signature '{raw}'"),
        })?;
        if gt_pos < lt_pos {
            return Err(GirError::CorruptObject {
                id: id_hint.to_string(),
                reason: format!("malformed '<...>' brackets in signature '{raw}'"),
            });
        }

        let name = raw[..lt_pos].trim().to_string();
        let email = raw[lt_pos + 1..gt_pos].trim().to_string();
        let remainder = raw[gt_pos + 1..].trim();

        let (ts_str, tz_str) =
            remainder
                .split_once(' ')
                .ok_or_else(|| GirError::CorruptObject {
                    id: id_hint.to_string(),
                    reason: format!("missing timestamp/timezone in signature '{raw}'"),
                })?;

        let timestamp: i64 = ts_str.parse().map_err(|_| GirError::CorruptObject {
            id: id_hint.to_string(),
            reason: format!("invalid unix timestamp '{ts_str}' in signature"),
        })?;

        Ok(Self {
            name,
            email,
            timestamp,
            tz_offset: tz_str.trim().to_string(),
        })
    }

    /// Formats this signature's timestamp and timezone offset into Git's human-readable date string
    /// (`"Sun Oct 27 10:33:20 2024 +0700"`).
    pub fn format_human_date(&self) -> String {
        let offset_secs = parse_tz_offset_secs(&self.tz_offset);
        let local_epoch = self.timestamp + offset_secs;

        unsafe {
            let time_val = local_epoch as libc::time_t;
            let mut tm: libc::tm = std::mem::zeroed();
            if libc::gmtime_r(&time_val, &mut tm).is_null() {
                return format!("{} {}", self.timestamp, self.tz_offset);
            }

            const WDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
            const MONTHS: [&str; 12] = [
                "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
            ];

            let wday = WDAYS.get(tm.tm_wday as usize).copied().unwrap_or("Sun");
            let mon = MONTHS.get(tm.tm_mon as usize).copied().unwrap_or("Jan");
            let mday = tm.tm_mday;
            let hour = tm.tm_hour;
            let min = tm.tm_min;
            let sec = tm.tm_sec;
            let year = tm.tm_year + 1900;

            format!(
                "{wday} {mon} {mday} {hour:02}:{min:02}:{sec:02} {year} {}",
                self.tz_offset
            )
        }
    }
}

fn parse_tz_offset_secs(tz: &str) -> i64 {
    let trimmed = tz.trim();
    if trimmed.len() != 5 {
        return 0;
    }
    let sign = match trimmed.as_bytes()[0] {
        b'-' => -1i64,
        _ => 1i64,
    };
    let hours: i64 = trimmed[1..3].parse().unwrap_or(0);
    let mins: i64 = trimmed[3..5].parse().unwrap_or(0);
    sign * (hours * 3600 + mins * 60)
}

impl fmt::Display for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} <{}> {} {}",
            self.name, self.email, self.timestamp, self.tz_offset
        )
    }
}

/// Queries `localtime_r` for the local UTC offset at `unix_secs` and formats it as `+HHMM` or `-HHMM`.
fn local_tz_offset_string(unix_secs: i64) -> String {
    unsafe {
        let time_val = unix_secs as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&time_val, &mut tm).is_null() {
            return "+0000".to_string();
        }
        let offset_secs = tm.tm_gmtoff;
        let sign = if offset_secs >= 0 { '+' } else { '-' };
        let total_mins = (offset_secs.abs() / 60) as i32;
        let hours = total_mins / 60;
        let mins = total_mins % 60;
        format!("{sign}{hours:02}{mins:02}")
    }
}

/// A Git `commit` object linking a root `Tree` snapshot to zero or more parent commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub tree: ObjectId,
    pub parents: Vec<ObjectId>,
    pub author: Signature,
    pub committer: Signature,
    pub message: String,
}

impl Commit {
    /// Parses a raw `commit` payload.
    pub fn parse(id_hint: &str, data: &[u8]) -> Result<Self, GirError> {
        let text = std::str::from_utf8(data).map_err(|_| GirError::CorruptObject {
            id: id_hint.to_string(),
            reason: "commit object is not valid UTF-8".to_string(),
        })?;

        let (header_block, message) = text.split_once("\n\n").unwrap_or((text, ""));

        let mut tree: Option<ObjectId> = None;
        let mut parents = Vec::new();
        let mut author: Option<Signature> = None;
        let mut committer: Option<Signature> = None;

        for line in header_block.lines() {
            if let Some(rest) = line.strip_prefix("tree ") {
                tree = Some(ObjectId::from_hex(rest.trim())?);
            } else if let Some(rest) = line.strip_prefix("parent ") {
                parents.push(ObjectId::from_hex(rest.trim())?);
            } else if let Some(rest) = line.strip_prefix("author ") {
                author = Some(Signature::parse(id_hint, rest)?);
            } else if let Some(rest) = line.strip_prefix("committer ") {
                committer = Some(Signature::parse(id_hint, rest)?);
            }
        }

        Ok(Self {
            tree: tree.ok_or_else(|| GirError::CorruptObject {
                id: id_hint.to_string(),
                reason: "commit missing 'tree' header".to_string(),
            })?,
            parents,
            author: author.ok_or_else(|| GirError::CorruptObject {
                id: id_hint.to_string(),
                reason: "commit missing 'author' header".to_string(),
            })?,
            committer: committer.ok_or_else(|| GirError::CorruptObject {
                id: id_hint.to_string(),
                reason: "commit missing 'committer' header".to_string(),
            })?,
            message: message.to_string(),
        })
    }

    /// Serializes this `Commit` into its canonical UTF-8 payload.
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = String::new();
        out.push_str(&format!("tree {}\n", self.tree));
        for parent in &self.parents {
            out.push_str(&format!("parent {}\n", parent));
        }
        out.push_str(&format!("author {}\n", self.author));
        out.push_str(&format!("committer {}\n", self.committer));
        out.push('\n');
        out.push_str(&self.message);
        out.into_bytes()
    }

    /// Converts this `Commit` into a `RawObject`.
    pub fn to_raw(&self) -> RawObject {
        RawObject {
            kind: ObjectKind::Commit,
            data: self.serialize(),
        }
    }

    /// Returns the first line of the commit message (the subject line).
    pub fn subject(&self) -> &str {
        self.message.lines().next().unwrap_or("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_roundtrip_serialization() {
        let tree = ObjectId::from_hex("4b825dc642cb6eb9a060e54bf8d69288fbee4904").unwrap();
        let parent = ObjectId::from_hex("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391").unwrap();
        let sig = Signature::new("Irfan", "mail@irfankurniawan.com", 1730000000, "+0700");

        let commit = Commit {
            tree,
            parents: vec![parent],
            author: sig.clone(),
            committer: sig,
            message: "feat: initial commit\n".to_string(),
        };

        let bytes = commit.serialize();
        let decoded = Commit::parse("test", &bytes).unwrap();
        assert_eq!(decoded, commit);
        assert_eq!(decoded.subject(), "feat: initial commit");
    }
}
