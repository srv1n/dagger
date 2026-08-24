//! Scope-confined key types.

use serde::{Deserialize, Deserializer, Serialize};
use std::marker::PhantomData;

/// A validated tenant or namespace component.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ScopeAtom(String);

impl ScopeAtom {
    /// Validates and constructs a scope atom.
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

    /// Returns the validated atom text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for ScopeAtom {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)
            .and_then(|value| Self::new(value).map_err(serde::de::Error::custom))
    }
}

/// A scope-atom validation failure.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ScopeAtomError {
    /// The atom is empty.
    #[error("scope atom is empty")]
    Empty,
    /// The atom exceeds 128 UTF-8 bytes.
    #[error("scope atom exceeds 128 bytes")]
    TooLong,
    /// The atom does not match the closed character grammar.
    #[error("scope atom has invalid characters")]
    InvalidCharacters,
}

/// The immutable tenant and namespace boundary.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionScope {
    /// Tenant component of the scope.
    pub tenant_id: ScopeAtom,
    /// Namespace component of the scope.
    pub namespace: ScopeAtom,
}

/// A logical identifier paired with its mandatory scope.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopedId<T> {
    /// Scope containing the identifier.
    pub scope: ExecutionScope,
    /// Identifier value within the scope.
    pub id: T,
    #[serde(skip)]
    marker: PhantomData<fn() -> T>,
}

impl<T> ScopedId<T> {
    /// Constructs an explicitly scoped identifier.
    pub fn new(scope: ExecutionScope, id: T) -> Self {
        Self {
            scope,
            id,
            marker: PhantomData,
        }
    }
}
