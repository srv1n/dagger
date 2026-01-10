//! Utility functions for the task-core system

use bytes::Bytes;
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::model::AgentError;

/// Get current timestamp in milliseconds
pub fn timestamp_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_millis() as u64
}

/// Format duration for display
pub fn format_duration(duration: std::time::Duration) -> String {
    let secs = duration.as_secs();
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

/// Convert common output types into bytes for task-core agents.
pub trait IntoBytes {
    fn into_bytes(self) -> Result<Bytes, AgentError>;
}

impl IntoBytes for Bytes {
    fn into_bytes(self) -> Result<Bytes, AgentError> {
        Ok(self)
    }
}

impl IntoBytes for Vec<u8> {
    fn into_bytes(self) -> Result<Bytes, AgentError> {
        Ok(Bytes::from(self))
    }
}

impl IntoBytes for String {
    fn into_bytes(self) -> Result<Bytes, AgentError> {
        Ok(Bytes::from(self))
    }
}

impl IntoBytes for &str {
    fn into_bytes(self) -> Result<Bytes, AgentError> {
        Ok(Bytes::from(self.to_string()))
    }
}

impl IntoBytes for Value {
    fn into_bytes(self) -> Result<Bytes, AgentError> {
        serde_json::to_vec(&self)
            .map(Bytes::from)
            .map_err(|e| AgentError::System(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(Duration::from_secs(45)), "45s");
        assert_eq!(format_duration(Duration::from_secs(90)), "1m 30s");
        assert_eq!(format_duration(Duration::from_secs(3661)), "1h 1m");
    }
}
