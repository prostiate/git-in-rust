use crate::error::GirError;
use crate::graph::peel_to_commit_object;
use crate::hash::ObjectId;
use crate::repository::Repository;
use crate::revision::parse_revision;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoAction {
    Pick,
    Reword,
    Edit,
    Squash,
    Fixup,
    Drop,
}

impl TodoAction {
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "pick" | "p" => Some(Self::Pick),
            "reword" | "r" => Some(Self::Reword),
            "edit" | "e" => Some(Self::Edit),
            "squash" | "s" => Some(Self::Squash),
            "fixup" | "f" => Some(Self::Fixup),
            "drop" | "d" => Some(Self::Drop),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pick => "pick",
            Self::Reword => "reword",
            Self::Edit => "edit",
            Self::Squash => "squash",
            Self::Fixup => "fixup",
            Self::Drop => "drop",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoItem {
    pub action: TodoAction,
    pub commit_oid: ObjectId,
    pub subject: String,
}

/// Formats a list of `TodoItem`s into `.git/rebase-merge/git-rebase-todo` syntax.
pub fn format_todo_script(items: &[TodoItem]) -> String {
    let mut out = String::new();
    for item in items {
        out.push_str(&format!(
            "{} {} {}\n",
            item.action.as_str(),
            item.commit_oid.short_hex(),
            item.subject
        ));
    }
    out
}

/// Parses a `git-rebase-todo` script, ignoring blank lines and `#` comment lines.
pub fn parse_todo_script(repo: &Repository, text: &str) -> Result<Vec<TodoItem>, GirError> {
    let mut items = Vec::new();
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.splitn(3, char::is_whitespace);
        let Some(cmd_str) = parts.next() else {
            continue;
        };
        let Some(action) = TodoAction::parse(cmd_str) else {
            return Err(GirError::InvalidArgument(format!(
                "invalid rebase todo command '{cmd_str}' in line: {line}"
            )));
        };
        let Some(rev_str) = parts.next().map(str::trim).filter(|s| !s.is_empty()) else {
            return Err(GirError::InvalidArgument(format!(
                "missing commit in rebase todo line: {line}"
            )));
        };
        let raw_oid = parse_revision(repo, rev_str)?;
        let (commit_oid, commit) = peel_to_commit_object(repo.odb(), &raw_oid)?;
        let subject = parts
            .next()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| commit.subject().to_string());

        items.push(TodoItem {
            action,
            commit_oid,
            subject,
        });
    }
    Ok(items)
}
