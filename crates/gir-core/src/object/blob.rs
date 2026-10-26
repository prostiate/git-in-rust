use crate::object::{ObjectKind, RawObject};

/// A Git `blob` object storing raw uninterpreted file contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blob {
    pub data: Vec<u8>,
}

impl Blob {
    pub fn new(data: impl Into<Vec<u8>>) -> Self {
        Self { data: data.into() }
    }

    pub fn to_raw(&self) -> RawObject {
        RawObject {
            kind: ObjectKind::Blob,
            data: self.data.clone(),
        }
    }
}
