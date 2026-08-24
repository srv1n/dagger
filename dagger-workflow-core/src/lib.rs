//! Public API for the Dagger workflow engine.
//!
//! This crate owns workflow definitions, execution, state, and storage boundaries.

#![deny(missing_docs)]
#![allow(async_fn_in_trait)]

pub mod action;
pub mod approval;
pub mod artifact;
pub mod budget;
pub mod committed_read;
#[cfg(feature = "conformance")]
pub mod conformance;
pub mod definition;
pub mod engine;
pub mod event;
pub mod fs_object_store;
pub mod ids;
pub mod memory;
pub mod revision;
pub mod run;
pub mod scope;
#[cfg(feature = "sqlite")]
pub mod sqlite;
pub mod store;
