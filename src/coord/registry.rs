//! Registry for NodeAction instances
//! 
//! This replaces the old function-based registry with a trait-based one.

use std::collections::HashMap;
use std::sync::Arc;
use parking_lot::RwLock;
use crate::coord::action::NodeAction;

/// Registry for node actions
#[derive(Clone)]
pub struct ActionRegistry {
    actions: Arc<RwLock<HashMap<String, Arc<dyn NodeAction>>>>,
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