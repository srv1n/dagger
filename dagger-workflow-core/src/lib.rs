//! Public specification skeleton for the frozen dagger workflow core v0.1 contract.
//!
//! This crate owns the shared API that W1 and W2 implement against.

#![deny(missing_docs)]
#![allow(async_fn_in_trait)]

pub mod action;
pub mod approval;
pub mod artifact;
pub mod budget;
pub mod definition;
pub mod engine;
pub mod event;
pub mod ids;
pub mod revision;
pub mod run;
pub mod scope;
pub mod store;
