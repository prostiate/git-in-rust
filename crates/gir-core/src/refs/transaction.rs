use crate::hash::ObjectId;

/// Compare-and-swap (CAS) precondition for reference updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpectedOldTarget {
    /// Do not verify the prior target (unconditional update).
    Any,
    /// Require the reference to not exist prior to the update (`0000...`).
    MustNotExist,
    /// Require the reference to currently point to this exact `ObjectId`.
    Exact(ObjectId),
}

impl ExpectedOldTarget {
    pub fn from_optional_oid(oid: Option<ObjectId>) -> Self {
        match oid {
            None => Self::Any,
            Some(id) if id.is_zero() => Self::MustNotExist,
            Some(id) => Self::Exact(id),
        }
    }
}
