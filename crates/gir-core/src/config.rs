use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::GirError;
use crate::storage::atomic::LockFile;

/// Section identifier in a Git INI configuration file (e.g., `[core]` or `[remote "origin"]`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConfigSection {
    pub name: String,
    pub subsection: Option<String>,
}

impl ConfigSection {
    pub fn new(name: impl Into<String>, subsection: Option<String>) -> Self {
        Self {
            name: name.into().to_ascii_lowercase(),
            subsection,
        }
    }

    pub fn format_header(&self) -> String {
        match &self.subsection {
            Some(sub) => format!("[{} \"{}\"]", self.name, sub),
            None => format!("[{}]", self.name),
        }
    }
}

/// Parser and writer for `.git/config` and `~/.gitconfig` INI files.
#[derive(Debug, Clone, Default)]
pub struct GitConfig {
    sections: BTreeMap<ConfigSection, BTreeMap<String, String>>,
    source_path: Option<PathBuf>,
}

impl GitConfig {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the path to the user's global `~/.gitconfig` file, if `HOME` is set.
    pub fn global_config_path() -> Option<PathBuf> {
        if let Ok(home) = env::var("HOME") {
            let gitconfig = PathBuf::from(&home).join(".gitconfig");
            if gitconfig.is_file() {
                return Some(gitconfig);
            }
            let xdg = PathBuf::from(&home).join(".config/git/config");
            if xdg.is_file() {
                return Some(xdg);
            }
            return Some(gitconfig);
        }
        None
    }

    /// Loads a `GitConfig` file from disk (returning an empty config if the file does not exist).
    pub fn load(path: impl AsRef<Path>) -> Result<Self, GirError> {
        let path = path.as_ref();
        if !path.is_file() {
            return Ok(Self {
                sections: BTreeMap::new(),
                source_path: Some(path.to_path_buf()),
            });
        }

        let content = fs::read_to_string(path)?;
        let mut cfg = Self::parse_str(&content)?;
        cfg.source_path = Some(path.to_path_buf());
        Ok(cfg)
    }

    /// Loads the cascaded configuration (`~/.gitconfig` overlaid by local `.git/config`).
    pub fn load_cascaded(local_config_path: Option<&Path>) -> Result<Self, GirError> {
        let mut merged = Self::new();
        if let Some(global_path) = Self::global_config_path() {
            if global_path.is_file() {
                let global_cfg = Self::load(&global_path)?;
                merged.merge_from(&global_cfg);
            }
        }
        if let Some(local_path) = local_config_path {
            if local_path.is_file() {
                let local_cfg = Self::load(local_path)?;
                merged.merge_from(&local_cfg);
            }
            merged.source_path = Some(local_path.to_path_buf());
        }
        Ok(merged)
    }

    /// Parses Git INI configuration text.
    pub fn parse_str(text: &str) -> Result<Self, GirError> {
        let mut sections: BTreeMap<ConfigSection, BTreeMap<String, String>> = BTreeMap::new();
        let mut current_section: Option<ConfigSection> = None;

        for raw_line in text.lines() {
            let line = strip_unquoted_comment(raw_line).trim();
            if line.is_empty() {
                continue;
            }

            if line.starts_with('[') && line.ends_with(']') {
                let inner = line[1..line.len() - 1].trim();
                let sec = parse_section_header(inner)?;
                sections.entry(sec.clone()).or_default();
                current_section = Some(sec);
                continue;
            }

            let Some(ref sec) = current_section else {
                continue;
            };

            if let Some((k, v)) = line.split_once('=') {
                let key = k.trim().to_ascii_lowercase();
                let mut val = v.trim().to_string();
                if val.len() >= 2 && val.starts_with('"') && val.ends_with('"') {
                    val = val[1..val.len() - 1].to_string();
                }
                sections.entry(sec.clone()).or_default().insert(key, val);
            } else {
                // Boolean shorthand key with no `=`: treated as `"true"` in Git config.
                let key = line.trim().to_ascii_lowercase();
                sections
                    .entry(sec.clone())
                    .or_default()
                    .insert(key, "true".to_string());
            }
        }

        Ok(Self {
            sections,
            source_path: None,
        })
    }

    /// Merges entries from `other` into `self` (overwriting matching keys).
    pub fn merge_from(&mut self, other: &Self) {
        for (sec, kv) in &other.sections {
            let target = self.sections.entry(sec.clone()).or_default();
            for (k, v) in kv {
                target.insert(k.clone(), v.clone());
            }
        }
    }

    /// Looks up a dotted config key such as `"user.name"` or `"remote.origin.url"`.
    pub fn get(&self, dotted_key: &str) -> Option<&str> {
        let (sec, key) = split_dotted_key(dotted_key).ok()?;
        self.sections
            .get(&sec)
            .and_then(|kv| kv.get(&key))
            .map(String::as_str)
    }

    /// Sets a dotted config key such as `"user.name"` or `"remote.origin.url"`.
    pub fn set(&mut self, dotted_key: &str, value: impl Into<String>) -> Result<(), GirError> {
        let (sec, key) = split_dotted_key(dotted_key)?;
        self.sections
            .entry(sec)
            .or_default()
            .insert(key, value.into());
        Ok(())
    }

    /// Removes a dotted config key if present.
    pub fn unset(&mut self, dotted_key: &str) -> Result<bool, GirError> {
        let (sec, key) = split_dotted_key(dotted_key)?;
        if let Some(kv) = self.sections.get_mut(&sec) {
            return Ok(kv.remove(&key).is_some());
        }
        Ok(false)
    }

    /// Removes an entire config section (e.g. `[branch "old"]`) and returns its key-value map.
    pub fn remove_section(
        &mut self,
        name: &str,
        subsection: Option<&str>,
    ) -> Option<BTreeMap<String, String>> {
        let sec = ConfigSection::new(name, subsection.map(ToString::to_string));
        self.sections.remove(&sec)
    }

    /// Returns a flattened list of `(dotted_key, value)` pairs for `gir config --list`.
    pub fn entries(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for (sec, kv) in &self.sections {
            for (k, v) in kv {
                let full_key = match &sec.subsection {
                    Some(sub) => format!("{}.{}.{}", sec.name, sub, k),
                    None => format!("{}.{}", sec.name, k),
                };
                out.push((full_key, v.clone()));
            }
        }
        out
    }

    /// Serializes this `GitConfig` back into standard Git INI format.
    pub fn to_ini_string(&self) -> String {
        let mut out = String::new();
        for (sec, kv) in &self.sections {
            if kv.is_empty() {
                continue;
            }
            out.push_str(&sec.format_header());
            out.push('\n');
            for (k, v) in kv {
                out.push_str(&format!("\t{} = {}\n", k, v));
            }
        }
        out
    }

    /// Atomically writes this config to `target_path` using `LockFile` (`<path>.lock`).
    pub fn save_to(&self, target_path: impl AsRef<Path>) -> Result<(), GirError> {
        let mut lock = LockFile::acquire(target_path)?;
        lock.write_all(self.to_ini_string().as_bytes())?;
        lock.commit()
    }
}

fn parse_section_header(inner: &str) -> Result<ConfigSection, GirError> {
    if let Some((sec_name, rest)) = inner.split_once(|c: char| c.is_ascii_whitespace()) {
        let rest = rest.trim();
        if rest.len() >= 2 && rest.starts_with('"') && rest.ends_with('"') {
            let sub = &rest[1..rest.len() - 1];
            return Ok(ConfigSection::new(sec_name.trim(), Some(sub.to_string())));
        }
        return Err(GirError::Config(format!(
            "invalid section header '[{inner}]'"
        )));
    }
    Ok(ConfigSection::new(inner, None))
}

fn split_dotted_key(dotted_key: &str) -> Result<(ConfigSection, String), GirError> {
    let trimmed = dotted_key.trim();
    let parts: Vec<&str> = trimmed.split('.').collect();
    if parts.len() < 2 {
        return Err(GirError::Config(format!(
            "key does not contain a section: {trimmed}"
        )));
    }
    if parts.len() == 2 {
        Ok((
            ConfigSection::new(parts[0], None),
            parts[1].to_ascii_lowercase(),
        ))
    } else {
        let sec_name = parts[0];
        let key_name = parts[parts.len() - 1].to_ascii_lowercase();
        let subsection = parts[1..parts.len() - 1].join(".");
        Ok((ConfigSection::new(sec_name, Some(subsection)), key_name))
    }
}

fn strip_unquoted_comment(line: &str) -> &str {
    let mut in_quotes = false;
    let mut escaped = false;
    for (idx, ch) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == '"' {
            in_quotes = !in_quotes;
            continue;
        }
        if !in_quotes && (ch == '#' || ch == ';') {
            return &line[..idx];
        }
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_serializes_sections_and_subsections() {
        let raw = r#"
# Core settings
[core]
	repositoryformatversion = 0
	filemode = true
	bare = false
[remote "origin"]
	url = https://github.com/prostiate/git-in-rust.git
	fetch = +refs/heads/*:refs/remotes/origin/*
[branch "main"]
	remote = origin
	merge = refs/heads/main
"#;
        let mut cfg = GitConfig::parse_str(raw).unwrap();
        assert_eq!(cfg.get("core.filemode"), Some("true"));
        assert_eq!(
            cfg.get("remote.origin.url"),
            Some("https://github.com/prostiate/git-in-rust.git")
        );
        assert_eq!(cfg.get("branch.main.remote"), Some("origin"));

        cfg.set("user.name", "Muhammad Irfan Kurniawan").unwrap();
        assert_eq!(cfg.get("user.name"), Some("Muhammad Irfan Kurniawan"));
    }
}
