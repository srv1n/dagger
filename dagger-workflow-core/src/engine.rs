//! Minimal engine construction surface reserved for W3. Contract sections 5 and 6.

use crate::action::ActionRegistry;
use crate::artifact::ObjectStore;
use crate::ids::Id;
use crate::store::WorkflowStore;
use std::sync::Arc;

/// Scheduler-wide non-durable execution configuration. Contract section 10.3.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineConfig {
    /// Engine instance label used by the singleton claim. Contract section 6.
    pub instance_id: Id,
    /// Maximum concurrently invoked actions in this process. Contract section 10.3.
    pub max_concurrency: usize,
}

/// Engine construction failure. Contract section 6.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum EngineBuildError {
    /// The configured concurrency is zero. Contract section 10.3.
    #[error("engine concurrency must be positive")]
    InvalidConcurrency,
}

/// Durable workflow scheduler shell implemented by W3. Contract section 5.
#[allow(dead_code)]
pub struct WorkflowEngine<S, O, R> {
    store: Arc<S>,
    object_store: Arc<O>,
    registry: Arc<R>,
    config: EngineConfig,
}

impl<S, O, R> WorkflowEngine<S, O, R>
where
    S: WorkflowStore,
    O: ObjectStore,
    R: ActionRegistry,
{
    /// Constructs an engine without acquiring its scoped claim. Contract section 6.
    pub fn new(
        _store: Arc<S>,
        _object_store: Arc<O>,
        _registry: Arc<R>,
        _config: EngineConfig,
    ) -> Result<Self, EngineBuildError> {
        todo!()
    }
}
