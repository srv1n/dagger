use std::collections::HashMap;
use std::fmt;
use thiserror::Error;

/// Unified error type for the entire Dagger library
#[derive(Debug, Error)]
pub enum DaggerError {
    /// Execution-related errors
    #[error("Execution failed in {component}: {message}")]
    Execution {
        component: String,
        message: String,
        context: HashMap<String, String>,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    /// Resource exhaustion errors
    #[error("Resource exhausted: {resource} (current: {current}, limit: {limit})")]
    ResourceExhaustion {
        resource: String,
        current: u64,
        limit: u64,
        details: Option<String>,
    },

    /// Validation errors
    #[error("Validation failed: {message}")]
    Validation {
        message: String,
        field: Option<String>,
        schema: Option<String>,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    /// Configuration errors
    #[error("Configuration error: {message}")]
    Configuration {
        message: String,
        field: Option<String>,
        expected: Option<String>,
        actual: Option<String>,
    },

    /// Database/persistence errors
    #[error("Database operation failed: {operation}")]
    Database {
        operation: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// Network/IO errors
    #[error("IO operation failed: {operation}")]
    Io {
        operation: String,
        #[source]
        source: std::io::Error,
    },

    /// Serialization errors
    #[error("Serialization failed: {format}")]
    Serialization {
        format: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// Concurrency errors (locks, timeouts, etc.)
    #[error("Concurrency error: {operation}")]
    Concurrency {
        operation: String,
        timeout_ms: Option<u64>,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    /// Task/workflow specific errors
    #[error("Task error: {task_id} - {message}")]
    Task {
        task_id: String,
        message: String,
        status: Option<String>,
        agent: Option<String>,
    },

    /// Agent-specific errors
    #[error("Agent error: {agent} - {message}")]
    Agent {
        agent: String,
        message: String,
        operation: Option<String>,
    },

    /// Channel/messaging errors
    #[error("Channel error: {channel} - {message}")]
    Channel {
        channel: String,
        message: String,
        operation: Option<String>,
    },

    /// Timeout errors
    #[error("Operation timed out: {operation} (timeout: {timeout_ms}ms)")]
    Timeout {
        operation: String,
        timeout_ms: u64,
    },

    /// Cancellation errors
    #[error("Operation was cancelled: {operation}")]
    Cancelled {
        operation: String,
        reason: Option<String>,
    },

    /// Generic internal errors
    #[error("Internal error: {message}")]
    Internal {
        message: String,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },
}

impl DaggerError {
    /// Create an execution error with context
    pub fn execution<S: Into<String>>(
        component: S,
        message: S,
    ) -> Self {
        Self::Execution {
            component: component.into(),
            message: message.into(),
            context: HashMap::new(),
            source: None,
        }
    }

    /// Create an execution error with source
    pub fn execution_with_source<S: Into<String>, E: std::error::Error + Send + Sync + 'static>(
        component: S,
        message: S,
        source: E,
    ) -> Self {
        Self::Execution {
            component: component.into(),
            message: message.into(),
            context: HashMap::new(),
            source: Some(Box::new(source)),
        }
    }

    /// Add context to an execution error
    pub fn with_context<K: Into<String>, V: Into<String>>(mut self, key: K, value: V) -> Self {
        if let Self::Execution { ref mut context, .. } = self {
            context.insert(key.into(), value.into());
        }
        self
    }

    /// Create a resource exhaustion error
    pub fn resource_exhausted<S: Into<String>>(
        resource: S,
        current: u64,
        limit: u64,
    ) -> Self {
        Self::ResourceExhaustion {
            resource: resource.into(),
            current,
            limit,
            details: None,
        }
    }

    /// Create a validation error
    pub fn validation<S: Into<String>>(message: S) -> Self {
        Self::Validation {
            message: message.into(),
            field: None,
            schema: None,
            source: None,
        }
    }

    /// Create a validation error with field
    pub fn validation_field<S: Into<String>, F: Into<String>>(
        message: S,
        field: F,
    ) -> Self {
        Self::Validation {
            message: message.into(),
            field: Some(field.into()),
            schema: None,
            source: None,
        }
    }

    /// Create a configuration error
    pub fn configuration<S: Into<String>>(message: S) -> Self {
        Self::Configuration {
            message: message.into(),
            field: None,
            expected: None,
            actual: None,
        }
    }

    /// Create a database error
    pub fn database<S: Into<String>, E: std::error::Error + Send + Sync + 'static>(
        operation: S,
        source: E,
    ) -> Self {
        Self::Database {
            operation: operation.into(),
            source: Box::new(source),
        }
    }

    /// Create an IO error
    pub fn io<S: Into<String>>(operation: S, source: std::io::Error) -> Self {
        Self::Io {
            operation: operation.into(),
            source,
        }
    }

    /// Create a serialization error
    pub fn serialization<S: Into<String>, E: std::error::Error + Send + Sync + 'static>(
        format: S,
        source: E,
    ) -> Self {
        Self::Serialization {
            format: format.into(),
            source: Box::new(source),
        }
    }

    /// Create a concurrency error
    pub fn concurrency<S: Into<String>>(operation: S) -> Self {
        Self::Concurrency {
            operation: operation.into(),
            timeout_ms: None,
            source: None,
        }
    }

    /// Create a task error
    pub fn task<S: Into<String>, M: Into<String>>(task_id: S, message: M) -> Self {
        Self::Task {
            task_id: task_id.into(),
            message: message.into(),
            status: None,
            agent: None,
        }
    }

    /// Create an agent error
    pub fn agent<S: Into<String>, M: Into<String>>(agent: S, message: M) -> Self {
        Self::Agent {
            agent: agent.into(),
            message: message.into(),
            operation: None,
        }
    }

    /// Create a channel error
    pub fn channel<S: Into<String>, M: Into<String>>(channel: S, message: M) -> Self {
        Self::Channel {
            channel: channel.into(),
            message: message.into(),
            operation: None,
        }
    }

    /// Create a timeout error
    pub fn timeout<S: Into<String>>(operation: S, timeout_ms: u64) -> Self {
        Self::Timeout {
            operation: operation.into(),
            timeout_ms,
        }
    }

    /// Create a cancellation error
    pub fn cancelled<S: Into<String>>(operation: S) -> Self {
        Self::Cancelled {
            operation: operation.into(),
            reason: None,
        }
    }

    /// Create an internal error
    pub fn internal<S: Into<String>>(message: S) -> Self {
        Self::Internal {
            message: message.into(),
            source: None,
        }
    }

    /// Check if error is recoverable
    pub fn is_recoverable(&self) -> bool {
        match self {
            Self::Timeout { .. } | Self::Concurrency { .. } | Self::Io { .. } => true,
            Self::ResourceExhaustion { .. } => true, // May be recoverable after cleanup
            Self::Validation { .. } | Self::Configuration { .. } => false,
            Self::Database { .. } => true, // Database might recover
            Self::Cancelled { .. } => false,
            _ => false,
        }
    }

    /// Get error category for metrics/logging
    pub fn category(&self) -> &'static str {
        match self {
            Self::Execution { .. } => "execution",
            Self::ResourceExhaustion { .. } => "resource",
            Self::Validation { .. } => "validation",
            Self::Configuration { .. } => "configuration",
            Self::Database { .. } => "database",
            Self::Io { .. } => "io",
            Self::Serialization { .. } => "serialization",
            Self::Concurrency { .. } => "concurrency",
            Self::Task { .. } => "task",
            Self::Agent { .. } => "agent",
            Self::Channel { .. } => "channel",
            Self::Timeout { .. } => "timeout",
            Self::Cancelled { .. } => "cancelled",
            Self::Internal { .. } => "internal",
        }
    }
}

/// Result type alias for convenience
pub type Result<T> = std::result::Result<T, DaggerError>;

/// Convert from common error types
impl From<std::io::Error> for DaggerError {
    fn from(err: std::io::Error) -> Self {
        Self::io("io_operation", err)
    }
}

impl From<serde_json::Error> for DaggerError {
    fn from(err: serde_json::Error) -> Self {
        Self::serialization("json", err)
    }
}

impl From<sled::Error> for DaggerError {
    fn from(err: sled::Error) -> Self {
        Self::database("sled_operation", err)
    }
}

impl From<anyhow::Error> for DaggerError {
    fn from(err: anyhow::Error) -> Self {
        Self::internal(err.to_string()).with_context("source", "anyhow")
    }
}

/// Error context builder for chaining operations
pub struct ErrorContext {
    context: HashMap<String, String>,
}

impl ErrorContext {
    pub fn new() -> Self {
        Self {
            context: HashMap::new(),
        }
    }

    pub fn add<K: Into<String>, V: Into<String>>(mut self, key: K, value: V) -> Self {
        self.context.insert(key.into(), value.into());
        self
    }

    pub fn apply_to_error(self, mut error: DaggerError) -> DaggerError {
        if let DaggerError::Execution { ref mut context, .. } = error {
            context.extend(self.context);
        }
        error
    }
}

/// Macro for creating errors with context
#[macro_export]
macro_rules! dagger_error {
    (execution, $component:expr, $message:expr) => {
        DaggerError::execution($component, $message)
    };
    (execution, $component:expr, $message:expr, $source:expr) => {
        DaggerError::execution_with_source($component, $message, $source)
    };
    (validation, $message:expr) => {
        DaggerError::validation($message)
    };
    (validation, $message:expr, $field:expr) => {
        DaggerError::validation_field($message, $field)
    };
    (resource, $resource:expr, $current:expr, $limit:expr) => {
        DaggerError::resource_exhausted($resource, $current, $limit)
    };
    (timeout, $operation:expr, $timeout_ms:expr) => {
        DaggerError::timeout($operation, $timeout_ms)
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_creation() {
        let err = DaggerError::execution("test_component", "test failed");
        assert!(matches!(err, DaggerError::Execution { .. }));
        assert_eq!(err.category(), "execution");
    }

    #[test]
    fn test_error_context() {
        let err = DaggerError::execution("test", "message")
            .with_context("key1", "value1")
            .with_context("key2", "value2");
        
        if let DaggerError::Execution { context, .. } = err {
            assert_eq!(context.get("key1"), Some(&"value1".to_string()));
            assert_eq!(context.get("key2"), Some(&"value2".to_string()));
        } else {
            panic!("Expected execution error");
        }
    }

    #[test]
    fn test_error_recoverability() {
        assert!(DaggerError::timeout("test", 1000).is_recoverable());
        assert!(!DaggerError::validation("test").is_recoverable());
        assert!(!DaggerError::configuration("test").is_recoverable());
    }

    #[test]
    fn test_macro() {
        let err = dagger_error!(execution, "component", "message");
        assert!(matches!(err, DaggerError::Execution { .. }));
        
        let err = dagger_error!(validation, "invalid input");
        assert!(matches!(err, DaggerError::Validation { .. }));
    }
}