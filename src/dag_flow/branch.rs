//! Branch state management for DAG Flow
//! 
//! Provides pause/resume/cancel functionality for workflow branches

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use parking_lot::RwLock;
use tokio::time::{sleep, Duration};

/// Branch execution status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BranchStatus {
    Running,
    Paused,
    Cancelled,
    Completed,
}

/// Branch state information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchState {
    pub status: BranchStatus,
    pub reason: Option<String>,
    pub paused_at: Option<u64>,
    pub resumed_at: Option<u64>,
}

impl BranchState {
    pub fn new() -> Self {
        Self {
            status: BranchStatus::Running,
            reason: None,
            paused_at: None,
            resumed_at: None,
        }
    }
    
    pub fn running() -> Self {
        Self::new()
    }
    
    pub fn paused(reason: Option<String>) -> Self {
        Self {
            status: BranchStatus::Paused,
            reason,
            paused_at: Some(crate::dag_flow::events::now_ms()),
            resumed_at: None,
        }
    }
    
    pub fn cancelled(reason: Option<String>) -> Self {
        Self {
            status: BranchStatus::Cancelled,
            reason,
            paused_at: None,
            resumed_at: None,
        }
    }
}

/// Branch registry for managing branch states
#[derive(Clone)]
pub struct BranchRegistry {
    branches: Arc<RwLock<HashMap<String, BranchState>>>,
}

impl BranchRegistry {
    pub fn new() -> Self {
        Self {
            branches: Arc::new(RwLock::new(HashMap::new())),
        }
    }
    
    /// Register a new branch
    pub fn register_branch(&self, branch_id: impl Into<String>) {
        let mut branches = self.branches.write();
        branches.insert(branch_id.into(), BranchState::running());
    }
    
    /// Pause a branch
    pub fn pause_branch(&self, branch_id: &str, reason: Option<&str>) {
        let mut branches = self.branches.write();
        if let Some(state) = branches.get_mut(branch_id) {
            state.status = BranchStatus::Paused;
            state.reason = reason.map(|s| s.to_string());
            state.paused_at = Some(crate::dag_flow::events::now_ms());
        }
    }
    
    /// Resume a branch
    pub fn resume_branch(&self, branch_id: &str) {
        let mut branches = self.branches.write();
        if let Some(state) = branches.get_mut(branch_id) {
            if state.status == BranchStatus::Paused {
                state.status = BranchStatus::Running;
                state.resumed_at = Some(crate::dag_flow::events::now_ms());
            }
        }
    }
    
    /// Cancel a branch
    pub fn cancel_branch(&self, branch_id: &str, reason: Option<&str>) {
        let mut branches = self.branches.write();
        if let Some(state) = branches.get_mut(branch_id) {
            state.status = BranchStatus::Cancelled;
            state.reason = reason.map(|s| s.to_string());
        }
    }
    
    /// Complete a branch
    pub fn complete_branch(&self, branch_id: &str) {
        let mut branches = self.branches.write();
        if let Some(state) = branches.get_mut(branch_id) {
            state.status = BranchStatus::Completed;
        }
    }
    
    /// Get branch status
    pub fn get_status(&self, branch_id: &str) -> Option<BranchStatus> {
        let branches = self.branches.read();
        branches.get(branch_id).map(|s| s.status)
    }
    
    /// Get branch state
    pub fn get_state(&self, branch_id: &str) -> Option<BranchState> {
        let branches = self.branches.read();
        branches.get(branch_id).cloned()
    }
    
    /// Check if branch is paused
    pub fn is_paused(&self, branch_id: &str) -> bool {
        self.get_status(branch_id) == Some(BranchStatus::Paused)
    }
    
    /// Check if branch is cancelled
    pub fn is_cancelled(&self, branch_id: &str) -> bool {
        self.get_status(branch_id) == Some(BranchStatus::Cancelled)
    }
    
    /// Check if branch is running
    pub fn is_running(&self, branch_id: &str) -> bool {
        self.get_status(branch_id) == Some(BranchStatus::Running)
    }
    
    /// Wait for branch to be resumed (async)
    pub async fn await_branch_resumed(&self, branch_id: &str, poll_ms: u64) {
        let poll_duration = Duration::from_millis(poll_ms);
        
        while self.is_paused(branch_id) {
            sleep(poll_duration).await;
        }
    }
    
    /// List all branches with their states
    pub fn list_branches(&self) -> HashMap<String, BranchState> {
        self.branches.read().clone()
    }
    
    /// Clear completed or cancelled branches
    pub fn cleanup_finished(&self) {
        let mut branches = self.branches.write();
        branches.retain(|_, state| {
            state.status != BranchStatus::Completed && state.status != BranchStatus::Cancelled
        });
    }
}

impl Default for BranchRegistry {
    fn default() -> Self {
        Self::new()
    }
}