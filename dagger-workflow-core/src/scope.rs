//! Scope-confined key types from contract sections 1.1, 1.13, and 16.

use serde::{Deserialize, Serialize};
use std::marker::PhantomData;

/// A validated tenant or namespace component. Contract section 1.1.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ScopeAtom(String);

impl ScopeAtom {
    /// Validates and constructs a scope atom. Contract section 1.1.
    pub fn new(value: impl Into<String>) -> Result<Self, ScopeAtomError> {
        let value = value.into();
        if value.is_empty() {
            return Err(ScopeAtomError::Empty);
        }
        if value.len() > 128 {
            return Err(ScopeAtomError::TooLong);
        }
        let mut chars = value.bytes();
        let first = chars.next().expect("checked non-empty");
        if !first.is_ascii_alphanumeric()
            || chars.any(|byte| {
                !(byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
            })
        {
            return Err(ScopeAtomError::InvalidCharacters);
        }
        Ok(Self(value))
    }

    /// Returns the validated atom text. Contract section 1.1.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A scope-atom validation failure. Contract section 1.1.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ScopeAtomError {
    /// The atom is empty. Contract section 1.1.
    #[error("scope atom is empty")]
    Empty,
    /// The atom exceeds 128 UTF-8 bytes. Contract section 1.1.
    #[error("scope atom exceeds 128 bytes")]
    TooLong,
    /// The atom does not match the closed character grammar. Contract section 1.1.
    #[error("scope atom has invalid characters")]
    InvalidCharacters,
}

/// The immutable tenant and namespace boundary. Contract section 1.13.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionScope {
    /// Tenant component of the scope. Contract section 1.1.
    pub tenant_id: ScopeAtom,
    /// Namespace component of the scope. Contract section 1.1.
    pub namespace: ScopeAtom,
}

/// A logical identifier paired with its mandatory scope. Contract section 1.1.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopedId<T> {
    /// Scope containing the identifier. Contract section 16.1.
    pub scope: ExecutionScope,
    /// Identifier value within the scope. Contract section 1.1.
    pub id: T,
    #[serde(skip)]
    marker: PhantomData<fn() -> T>,
}

impl<T> ScopedId<T> {
    /// Constructs an explicitly scoped identifier. Contract section 1.13.
    pub fn new(scope: ExecutionScope, id: T) -> Self {
        Self {
            scope,
            id,
            marker: PhantomData,
        }
    }
}
