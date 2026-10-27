pub mod ascii_graph;

use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};

use crate::error::GirError;
use crate::hash::ObjectId;
use crate::object::commit::Commit;
use crate::object::ParsedObject;
use crate::refs::RefTarget;
use crate::repository::Repository;
use crate::revision::{parse_revision, parse_revision_spec, RevisionSpec};
use crate::storage::ObjectDatabase;

pub use ascii_graph::AsciiGraphRenderer;

/// A single commit yielded by `RevWalk`.
#[derive(Debug, Clone)]
pub struct RevWalkItem {
    pub oid: ObjectId,
    pub commit: Commit,
}

#[derive(Debug, Clone)]
struct HeapEntry {
    timestamp: i64,
    seq: Reverse<usize>,
    oid: ObjectId,
    commit: Commit,
}

impl PartialEq for HeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.timestamp == other.timestamp && self.seq == other.seq && self.oid == other.oid
    }
}

impl Eq for HeapEntry {}

impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        self.timestamp
            .cmp(&other.timestamp)
            .then_with(|| self.seq.cmp(&other.seq))
            .then_with(|| self.oid.cmp(&other.oid))
    }
}

/// Peels an `ObjectId` (through any annotated `Tag` layers) to a `Commit` `ObjectId` and `Commit`.
pub fn peel_to_commit_object(
    odb: &ObjectDatabase,
    oid: &ObjectId,
) -> Result<(ObjectId, Commit), GirError> {
    let mut current = *oid;
    for _ in 0..10 {
        match odb.read_parsed(&current)? {
            ParsedObject::Commit(c) => return Ok((current, c)),
            ParsedObject::Tag(t) => {
                current = t.object;
            }
            other => {
                return Err(GirError::UnexpectedObjectType {
                    expected: "commit".to_string(),
                    found: other.kind().to_string(),
                });
            }
        }
    }
    Err(GirError::InvalidRevision(oid.to_hex()))
}

/// Computes the set of all commit `ObjectId`s reachable from `roots` (inclusive).
pub fn reachable_commits(
    odb: &ObjectDatabase,
    roots: &[ObjectId],
) -> Result<HashSet<ObjectId>, GirError> {
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();

    for root in roots {
        if let Ok((commit_oid, _)) = peel_to_commit_object(odb, root) {
            if visited.insert(commit_oid) {
                queue.push_back(commit_oid);
            }
        }
    }

    while let Some(curr) = queue.pop_front() {
        let ParsedObject::Commit(commit) = odb.read_parsed(&curr)? else {
            continue;
        };
        for parent in commit.parents {
            if visited.insert(parent) {
                queue.push_back(parent);
            }
        }
    }

    Ok(visited)
}

/// Returns `true` if `ancestor` is an ancestor of (or equal to) `descendant`.
pub fn is_ancestor(
    odb: &ObjectDatabase,
    ancestor: &ObjectId,
    descendant: &ObjectId,
) -> Result<bool, GirError> {
    let (anc_oid, _) = peel_to_commit_object(odb, ancestor)?;
    let (desc_oid, _) = peel_to_commit_object(odb, descendant)?;
    if anc_oid == desc_oid {
        return Ok(true);
    }
    let reachable = reachable_commits(odb, &[desc_oid])?;
    Ok(reachable.contains(&anc_oid))
}

/// Directed Acyclic Graph (DAG) commit walker supporting chronological and topological (`--topo-order`) traversal.
pub struct RevWalk {
    items: VecDeque<RevWalkItem>,
}

impl RevWalk {
    /// Builds a `RevWalk` from optional revision arguments (`HEAD`, `A..B`, `A...B`, `^A B`).
    pub fn from_revisions(
        repo: &Repository,
        revs: &[String],
        topo_order: bool,
    ) -> Result<Self, GirError> {
        let odb = repo.odb();
        let mut positive_roots = Vec::new();
        let mut excluded_set = HashSet::new();

        if revs.is_empty() {
            match repo.refs().resolve_to_oid("HEAD") {
                Ok(head_oid) => {
                    let (c_oid, _) = peel_to_commit_object(odb, &head_oid)?;
                    positive_roots.push(c_oid);
                }
                Err(_) => {
                    let branch = repo
                        .refs()
                        .read_symbolic_ref("HEAD")
                        .unwrap_or_else(|_| "HEAD".to_string());
                    let short = branch.strip_prefix("refs/heads/").unwrap_or(&branch);
                    return Err(GirError::InvalidArgument(format!(
                        "your current branch '{short}' does not have any commits yet"
                    )));
                }
            }
        } else {
            let mut negative_roots = Vec::new();
            for raw_rev in revs {
                if let Some(neg_spec) = raw_rev.strip_prefix('^') {
                    let oid = parse_revision(repo, neg_spec)?;
                    let (c_oid, _) = peel_to_commit_object(odb, &oid)?;
                    negative_roots.push(c_oid);
                } else {
                    match parse_revision_spec(repo, raw_rev)? {
                        RevisionSpec::Single(oid) => {
                            let (c_oid, _) = peel_to_commit_object(odb, &oid)?;
                            positive_roots.push(c_oid);
                        }
                        RevisionSpec::Range { from, to } => {
                            let (from_c, _) = peel_to_commit_object(odb, &from)?;
                            let (to_c, _) = peel_to_commit_object(odb, &to)?;
                            negative_roots.push(from_c);
                            positive_roots.push(to_c);
                        }
                        RevisionSpec::SymmetricRange { left, right } => {
                            let (left_c, _) = peel_to_commit_object(odb, &left)?;
                            let (right_c, _) = peel_to_commit_object(odb, &right)?;
                            let reach_left = reachable_commits(odb, &[left_c])?;
                            let reach_right = reachable_commits(odb, &[right_c])?;
                            for common in reach_left.intersection(&reach_right) {
                                excluded_set.insert(*common);
                            }
                            positive_roots.push(left_c);
                            positive_roots.push(right_c);
                        }
                    }
                }
            }

            if !negative_roots.is_empty() {
                let neg_reach = reachable_commits(odb, &negative_roots)?;
                excluded_set.extend(neg_reach);
            }
        }

        Self::build(odb, &positive_roots, &excluded_set, topo_order)
    }

    /// Builds a `RevWalk` directly from positive root commits and an excluded commit set.
    pub fn build(
        odb: &ObjectDatabase,
        positive_roots: &[ObjectId],
        excluded_set: &HashSet<ObjectId>,
        topo_order: bool,
    ) -> Result<Self, GirError> {
        if topo_order {
            Self::build_topological(odb, positive_roots, excluded_set)
        } else {
            Self::build_chronological(odb, positive_roots, excluded_set)
        }
    }

    fn build_chronological(
        odb: &ObjectDatabase,
        positive_roots: &[ObjectId],
        excluded_set: &HashSet<ObjectId>,
    ) -> Result<Self, GirError> {
        let mut seen = excluded_set.clone();
        let mut heap = BinaryHeap::new();
        let mut seq = 0usize;

        for root in positive_roots {
            if seen.insert(*root) {
                let (_, commit) = peel_to_commit_object(odb, root)?;
                heap.push(HeapEntry {
                    timestamp: commit.committer.timestamp,
                    seq: Reverse(seq),
                    oid: *root,
                    commit,
                });
                seq += 1;
            }
        }

        let mut items = VecDeque::new();
        while let Some(entry) = heap.pop() {
            for parent_oid in &entry.commit.parents {
                if seen.insert(*parent_oid) {
                    let (_, parent_commit) = peel_to_commit_object(odb, parent_oid)?;
                    heap.push(HeapEntry {
                        timestamp: parent_commit.committer.timestamp,
                        seq: Reverse(seq),
                        oid: *parent_oid,
                        commit: parent_commit,
                    });
                    seq += 1;
                }
            }
            items.push_back(RevWalkItem {
                oid: entry.oid,
                commit: entry.commit,
            });
        }

        Ok(Self { items })
    }

    fn build_topological(
        odb: &ObjectDatabase,
        positive_roots: &[ObjectId],
        excluded_set: &HashSet<ObjectId>,
    ) -> Result<Self, GirError> {
        let mut commits_map: HashMap<ObjectId, Commit> = HashMap::new();
        let mut in_degree: HashMap<ObjectId, usize> = HashMap::new();
        let mut queue = VecDeque::new();

        for root in positive_roots {
            if !excluded_set.contains(root) && !commits_map.contains_key(root) {
                let (_, commit) = peel_to_commit_object(odb, root)?;
                commits_map.insert(*root, commit);
                in_degree.entry(*root).or_insert(0);
                queue.push_back(*root);
            }
        }

        while let Some(curr) = queue.pop_front() {
            let parents = commits_map
                .get(&curr)
                .map(|c| c.parents.clone())
                .unwrap_or_default();
            for p in parents {
                if excluded_set.contains(&p) {
                    continue;
                }
                *in_degree.entry(p).or_insert(0) += 1;
                if !commits_map.contains_key(&p) {
                    let (_, p_commit) = peel_to_commit_object(odb, &p)?;
                    commits_map.insert(p, p_commit);
                    queue.push_back(p);
                }
            }
        }

        let mut heap = BinaryHeap::new();
        let mut seq = 0usize;
        for (oid, &deg) in &in_degree {
            if deg == 0 {
                if let Some(commit) = commits_map.get(oid).cloned() {
                    heap.push(HeapEntry {
                        timestamp: commit.committer.timestamp,
                        seq: Reverse(seq),
                        oid: *oid,
                        commit,
                    });
                    seq += 1;
                }
            }
        }

        let mut items = VecDeque::new();
        while let Some(entry) = heap.pop() {
            for p in &entry.commit.parents {
                if let Some(deg) = in_degree.get_mut(p) {
                    *deg = deg.saturating_sub(1);
                    if *deg == 0 {
                        if let Some(p_commit) = commits_map.get(p).cloned() {
                            heap.push(HeapEntry {
                                timestamp: p_commit.committer.timestamp,
                                seq: Reverse(seq),
                                oid: *p,
                                commit: p_commit,
                            });
                            seq += 1;
                        }
                    }
                }
            }
            items.push_back(RevWalkItem {
                oid: entry.oid,
                commit: entry.commit,
            });
        }

        Ok(Self { items })
    }
}

impl Iterator for RevWalk {
    type Item = RevWalkItem;

    fn next(&mut self) -> Option<Self::Item> {
        self.items.pop_front()
    }
}

/// Builds a map from commit `ObjectId` to its decoration strings
/// (e.g. `["HEAD -> main", "tag: v1.0.0", "origin/main"]`).
pub fn build_ref_decorations(
    repo: &Repository,
) -> Result<HashMap<ObjectId, Vec<String>>, GirError> {
    let refs = repo.refs();
    let head_raw = refs.read_raw("HEAD")?;
    let head_sym = match head_raw {
        Some(RefTarget::Symbolic(ref s)) => Some(s.clone()),
        _ => None,
    };

    let mut map: HashMap<ObjectId, Vec<String>> = HashMap::new();
    let all_refs = refs.list_all()?;

    // 1. Add `HEAD -> <branch>` or detached `HEAD` first
    if let Ok(head_oid) = refs.resolve_to_oid("HEAD") {
        if let Ok((commit_oid, _)) = peel_to_commit_object(repo.odb(), &head_oid) {
            if let Some(ref sym) = head_sym {
                let short = sym.strip_prefix("refs/heads/").unwrap_or(sym);
                map.entry(commit_oid)
                    .or_default()
                    .push(format!("HEAD -> {short}"));
            } else {
                map.entry(commit_oid).or_default().push("HEAD".to_string());
            }
        }
    }

    // 2. Add local branches, tags, and remote-tracking branches
    for (ref_name, oid) in all_refs {
        if ref_name == "HEAD" || ref_name.starts_with("refs/gir/") {
            continue;
        }
        if Some(&ref_name) == head_sym.as_ref() {
            continue;
        }
        let Ok((commit_oid, _)) = peel_to_commit_object(repo.odb(), &oid) else {
            continue;
        };

        if let Some(branch) = ref_name.strip_prefix("refs/heads/") {
            map.entry(commit_oid).or_default().push(branch.to_string());
        } else if let Some(tag) = ref_name.strip_prefix("refs/tags/") {
            map.entry(commit_oid)
                .or_default()
                .push(format!("tag: {tag}"));
        } else if let Some(remote) = ref_name.strip_prefix("refs/remotes/") {
            map.entry(commit_oid).or_default().push(remote.to_string());
        }
    }

    Ok(map)
}
