//! Registry for NodeAction instances
//!
//! This replaces the old function-based registry with a trait-based one.

use crate::coord::action::NodeAction;
use linkme::distributed_slice;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

/// Registry for node actions
#[derive(Clone)]
pub struct ActionRegistry {
    actions: Arc<RwLock<HashMap<String, Arc<dyn NodeAction>>>>,
}

/// Global action registrars for compile-time registration
#[distributed_slice]
pub static ACTION_REGISTRARS: [fn(&ActionRegistry)] = [..];

/// Register all globally linked actions into a registry
pub fn register_global_actions(registry: &ActionRegistry) {
    for register in ACTION_REGISTRARS {
        register(registry);
    }
}

impl ActionRegistry {
    /// Create a new empty registry
    pub fn new() -> Self {
        Self {
            actions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register an action
    pub fn register(&self, action: Arc<dyn NodeAction>) {
        let mut actions = self.actions.write();
        actions.insert(action.name().to_string(), action);
    }

    /// Get an action by name
    pub fn get(&self, name: &str) -> Option<Arc<dyn NodeAction>> {
        let actions = self.actions.read();
        actions.get(name).cloned()
    }

    /// Check if an action is registered
    pub fn contains(&self, name: &str) -> bool {
        let actions = self.actions.read();
        actions.contains_key(name)
    }

    /// List all registered action names
    pub fn list(&self) -> Vec<String> {
        let actions = self.actions.read();
        actions.keys().cloned().collect()
    }
}

impl Default for ActionRegistry {
    fn default() -> Self {
        Self::new()
    }
}
