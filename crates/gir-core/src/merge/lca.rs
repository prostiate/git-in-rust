use std::collections::{HashMap, HashSet, VecDeque};

use crate::error::GirError;
use crate::graph::{peel_to_commit_object, reachable_commits};
use crate::hash::ObjectId;
use crate::object::ParsedObject;
use crate::storage::ObjectDatabase;

/// Finds all best common ancestors (Lowest Common Ancestors / `merge-base --all`) between
/// `commit_a` and `commit_b` in `O(V + E)` time.
///
/// If `--all` is not requested by a caller, the first element (`.first()`) is the primary merge base.
pub fn find_merge_bases(
    odb: &ObjectDatabase,
    commit_a: &ObjectId,
    commit_b: &ObjectId,
) -> Result<Vec<ObjectId>, GirError> {
    let (a_oid, _) = peel_to_commit_object(odb, commit_a)?;
    let (b_oid, _) = peel_to_commit_object(odb, commit_b)?;

    if a_oid == b_oid {
        return Ok(vec![a_oid]);
    }

    let anc_a = reachable_commits(odb, &[a_oid])?;
    let anc_b = reachable_commits(odb, &[b_oid])?;

    let common: HashSet<ObjectId> = anc_a.intersection(&anc_b).copied().collect();
    if common.is_empty() {
        return Ok(Vec::new());
    }

    // Seed a single BFS with the direct parents of all nodes in `common`.
    // In a DAG, the set of nodes reachable in >= 0 steps from `U_{y in common} parents(y)`
    // is the exact set of proper ancestors of `common` (dominated common ancestors).
    let mut dominated: HashSet<ObjectId> = HashSet::new();
    let mut queue: VecDeque<ObjectId> = VecDeque::new();
    let mut commit_timestamps: HashMap<ObjectId, i64> = HashMap::new();

    for &c_oid in &common {
        if let ParsedObject::Commit(c) = odb.read_parsed(&c_oid)? {
            commit_timestamps.insert(c_oid, c.committer.timestamp);
            for p in c.parents {
                if dominated.insert(p) {
                    queue.push_back(p);
                }
            }
        }
    }

    while let Some(curr) = queue.pop_front() {
        let ParsedObject::Commit(c) = odb.read_parsed(&curr)? else {
            continue;
        };
        for p in c.parents {
            if dominated.insert(p) {
                queue.push_back(p);
            }
        }
    }

    let mut bases: Vec<ObjectId> = common
        .into_iter()
        .filter(|oid| !dominated.contains(oid))
        .collect();

    // Sort by committer timestamp descending, then OID tie-breaker
    bases.sort_by(|x, y| {
        let tx = commit_timestamps.get(x).copied().unwrap_or(0);
        let ty = commit_timestamps.get(y).copied().unwrap_or(0);
        ty.cmp(&tx).then_with(|| x.cmp(y))
    });

    Ok(bases)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::commit::{Commit, Signature};
    use tempfile::TempDir;

    fn make_commit(odb: &ObjectDatabase, parents: Vec<ObjectId>, ts: i64, msg: &str) -> ObjectId {
        let tree_oid = odb.write_raw(crate::object::ObjectKind::Tree, &[]).unwrap();
        let sig = Signature::new("Test", "test@example.com", ts, "+0000");
        let c = Commit {
            tree: tree_oid,
            parents,
            author: sig.clone(),
            committer: sig,
            message: format!("{msg}\n"),
        };
        odb.write_object(&c.to_raw()).unwrap()
    }

    #[test]
    fn merge_base_on_diamond_and_criss_cross_dags() {
        let dir = TempDir::new().unwrap();
        let repo = crate::repository::Repository::init(dir.path(), true).unwrap();
        let odb = repo.odb();

        // Root R -> A1, B1
        let r = make_commit(odb, vec![], 100, "R");
        let a1 = make_commit(odb, vec![r], 200, "A1");
        let b1 = make_commit(odb, vec![r], 201, "B1");

        // Standard diamond: merge_base(A1, B1) == [R]
        assert_eq!(find_merge_bases(odb, &a1, &b1).unwrap(), vec![r]);

        // Criss-cross: A2 merges [A1, B1], B2 merges [B1, A1]
        let a2 = make_commit(odb, vec![a1, b1], 300, "A2");
        let b2 = make_commit(odb, vec![b1, a1], 301, "B2");

        let criss_bases = find_merge_bases(odb, &a2, &b2).unwrap();
        assert_eq!(criss_bases.len(), 2);
        assert!(criss_bases.contains(&a1));
        assert!(criss_bases.contains(&b1));
    }
}
