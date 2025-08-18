//! Event system for DAG Flow
//! 
//! Provides typed event emission for runtime events

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Runtime event types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum RuntimeEvent {
    StepPlanned {
        step_id: String,
        action: String,
        dependencies: Vec<String>,
    },
    StepStarted {
        step_id: String,
        action: String,
    },
    StepCompleted {
        step_id: String,
        success: bool,
        duration_ms: u64,
    },
    LlmToken {
        step_id: String,
        token: String,
    },
    ToolInvoked {
        step_id: String,
        tool_name: String,
        args: serde_json::Value,
    },
    BranchStateUpdated {
        branch_id: String,
        status: String,
        reason: Option<String>,
    },
}

/// Event envelope with metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeEventEnvelope {
    pub version: u32,
    pub sequence: u64,
    pub run_id: String,
    pub timestamp: u64,
    pub event: RuntimeEvent,
}

/// Event sink trait for emitting events
pub trait EventSink: Send + Sync {
    /// Emit an event
    fn emit(&self, envelope: &RuntimeEventEnvelope);
}

/// A simple logging event sink
pub struct LoggingEventSink;

impl EventSink for LoggingEventSink {
    fn emit(&self, envelope: &RuntimeEventEnvelope) {
        tracing::debug!("Event: {:?}", envelope);
    }
}

/// A buffering event sink that collects events
pub struct BufferingEventSink {
    events: Arc<parking_lot::RwLock<Vec<RuntimeEventEnvelope>>>,
}

impl BufferingEventSink {
    pub fn new() -> Self {
        Self {
            events: Arc::new(parking_lot::RwLock::new(Vec::new())),
        }
    }
    
    pub fn get_events(&self) -> Vec<RuntimeEventEnvelope> {
        self.events.read().clone()
    }
    
    pub fn clear(&self) {
        self.events.write().clear();
    }
}

impl EventSink for BufferingEventSink {
    fn emit(&self, envelope: &RuntimeEventEnvelope) {
        self.events.write().push(envelope.clone());
    }
}

/// Global sequence counter for events
static EVENT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Get the next event sequence number
pub fn next_sequence() -> u64 {
    EVENT_SEQUENCE.fetch_add(1, Ordering::SeqCst)
}

/// Get current timestamp in milliseconds
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

/// Helper macro for emitting V2 runtime events
#[macro_export]
macro_rules! emit_v2 {
    ($exec:expr, $run_id:expr, $event:expr) => {{
        if let Some(sink) = &$exec.event_sink {
            let envelope = $crate::dag_flow::events::RuntimeEventEnvelope {
                version: 2,
                sequence: $crate::dag_flow::events::next_sequence(),
                run_id: $run_id.to_string(),
                timestamp: $crate::dag_flow::events::now_ms(),
                event: $event,
            };
            sink.emit(&envelope);
        }
    }};
}