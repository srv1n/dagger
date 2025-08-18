// use crate::core::builders::DaggerConfig;
use crate::core::errors::{DaggerError, Result};
use crate::core::limits::ResourceTracker;
use crate::core::memory::Cache;
// use crate::core::performance::PerformanceMonitor;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;

/// PubSub system configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PubSubConfig {
    /// System name/identifier
    pub name: String,
    /// System description
    pub description: Option<String>,
    /// Maximum number of channels
    pub max_channels: usize,
    /// Maximum subscribers per channel
    pub max_subscribers_per_channel: usize,
    /// Maximum message size in bytes
    pub max_message_size_bytes: usize,
    /// Message retention policy
    pub retention_policy: MessageRetentionPolicy,
    /// Delivery guarantees
    pub delivery_guarantees: DeliveryGuarantees,
    /// Message ordering guarantees
    pub ordering: MessageOrdering,
    /// Performance settings
    pub performance: PubSubPerformanceConfig,
    /// Security settings
    pub security: PubSubSecurityConfig,
    /// Custom metadata
    pub metadata: HashMap<String, Value>,
}

impl Default for PubSubConfig {
    fn default() -> Self {
        Self {
            name: format!("pubsub_{}", Uuid::new_v4()),
            description: None,
            max_channels: 1000,
            max_subscribers_per_channel: 100,
            max_message_size_bytes: 1024 * 1024, // 1MB
            retention_policy: MessageRetentionPolicy::default(),
            delivery_guarantees: DeliveryGuarantees::AtLeastOnce,
            ordering: MessageOrdering::None,
            performance: PubSubPerformanceConfig::default(),
            security: PubSubSecurityConfig::default(),
            metadata: HashMap::new(),
        }
    }
}

/// Message retention policy
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageRetentionPolicy {
    /// Maximum number of messages to retain per channel
    pub max_messages: usize,
    /// Maximum time to retain messages
    pub max_age: Duration,
    /// Maximum storage size for retained messages
    pub max_storage_bytes: u64,
    /// Cleanup strategy when limits are exceeded
    pub cleanup_strategy: CleanupStrategy,
}

impl Default for MessageRetentionPolicy {
    fn default() -> Self {
        Self {
            max_messages: 10000,
            max_age: Duration::from_secs(3600 * 24), // 24 hours
            max_storage_bytes: 100 * 1024 * 1024,    // 100MB
            cleanup_strategy: CleanupStrategy::OldestFirst,
        }
    }
}

/// Cleanup strategy for message retention
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CleanupStrategy {
    /// Remove oldest messages first
    OldestFirst,
    /// Remove messages with lowest priority first
    LowestPriorityFirst,
    /// Remove largest messages first
    LargestFirst,
    /// Custom cleanup logic
    Custom(String),
}

/// Delivery guarantees
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DeliveryGuarantees {
    /// Fire and forget - no delivery confirmation
    AtMostOnce,
    /// Guaranteed delivery, may deliver multiple times
    AtLeastOnce,
    /// Guaranteed exactly once delivery
    ExactlyOnce,
}

/// Message ordering guarantees
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MessageOrdering {
    /// No ordering guarantees
    None,
    /// Messages are delivered in publish order per publisher
    PerPublisher,
    /// Global ordering across all publishers (performance impact)
    Global,
    /// Partitioned ordering based on message key
    Partitioned,
}

/// PubSub performance configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PubSubPerformanceConfig {
    /// Buffer size for message channels
    pub channel_buffer_size: usize,
    /// Batch size for message processing
    pub batch_size: usize,
    /// Flush interval for batched operations
    pub flush_interval: Duration,
    /// Number of worker threads for message processing
    pub worker_threads: usize,
    /// Enable message compression
    pub enable_compression: bool,
    /// Compression threshold in bytes
    pub compression_threshold: usize,
    /// Enable async message persistence
    pub async_persistence: bool,
}

impl Default for PubSubPerformanceConfig {
    fn default() -> Self {
        Self {
            channel_buffer_size: 1000,
            batch_size: 100,
            flush_interval: Duration::from_millis(100),
            worker_threads: 4,
            enable_compression: false,
            compression_threshold: 1024, // 1KB
            async_persistence: true,
        }
    }
}

/// PubSub security configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PubSubSecurityConfig {
    /// Enable authentication for publishers
    pub enable_publisher_auth: bool,
    /// Enable authentication for subscribers
    pub enable_subscriber_auth: bool,
    /// Enable message encryption
    pub enable_encryption: bool,
    /// Enable access control lists
    pub enable_acl: bool,
    /// Message validation schema
    pub message_validation: Option<String>,
    /// Rate limiting settings
    pub rate_limiting: Option<RateLimitConfig>,
}

impl Default for PubSubSecurityConfig {
    fn default() -> Self {
        Self {
            enable_publisher_auth: false,
            enable_subscriber_auth: false,
            enable_encryption: false,
            enable_acl: false,
            message_validation: None,
            rate_limiting: None,
        }
    }
}

/// Rate limiting configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitConfig {
    /// Maximum messages per second per publisher
    pub max_messages_per_sec: u32,
    /// Maximum bytes per second per publisher
    pub max_bytes_per_sec: u64,
    /// Rate limiting window size
    pub window_size: Duration,
    /// Action to take when rate limit is exceeded
    pub action: RateLimitAction,
}

/// Action to take when rate limit is exceeded
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RateLimitAction {
    /// Drop the message
    Drop,
    /// Queue the message with delay
    Queue,
    /// Reject with error
    Reject,
}

/// Channel configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelConfig {
    /// Channel name
    pub name: String,
    /// Channel description
    pub description: Option<String>,
    /// Channel-specific retention policy
    pub retention_policy: Option<MessageRetentionPolicy>,
    /// Channel-specific delivery guarantees
    pub delivery_guarantees: Option<DeliveryGuarantees>,
    /// Whether the channel is persistent
    pub persistent: bool,
    /// Message schema for validation
    pub message_schema: Option<Value>,
    /// Channel metadata
    pub metadata: HashMap<String, Value>,
}

impl ChannelConfig {
    pub fn new<S: Into<String>>(name: S) -> Self {
        Self {
            name: name.into(),
            description: None,
            retention_policy: None,
            delivery_guarantees: None,
            persistent: true,
            message_schema: None,
            metadata: HashMap::new(),
        }
    }
}

/// Subscription configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscriptionConfig {
    /// Subscription name
    pub name: String,
    /// Channel to subscribe to
    pub channel: String,
    /// Message filter (if any)
    pub filter: Option<String>,
    /// Subscription type
    pub subscription_type: SubscriptionType,
    /// Starting position in the message stream
    pub start_position: StartPosition,
    /// Maximum unacknowledged messages
    pub max_unacked_messages: usize,
    /// Acknowledgment timeout
    pub ack_timeout: Duration,
    /// Subscription metadata
    pub metadata: HashMap<String, Value>,
}

/// Type of subscription
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SubscriptionType {
    /// Exclusive subscription - only one consumer
    Exclusive,
    /// Shared subscription - round-robin between consumers
    Shared,
    /// Failover subscription - one active, others standby
    Failover,
    /// Key shared - messages with same key go to same consumer
    KeyShared,
}

/// Starting position for subscription
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StartPosition {
    /// Start from earliest available message
    Earliest,
    /// Start from latest message
    Latest,
    /// Start from specific message ID
    MessageId(String),
    /// Start from specific timestamp
    Timestamp(u64),
}

/// Message structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// Unique message ID
    pub id: String,
    /// Message key (for partitioning/ordering)
    pub key: Option<String>,
    /// Message payload
    pub payload: Value,
    /// Message headers
    pub headers: HashMap<String, String>,
    /// Message timestamp
    pub timestamp: u64,
    /// Message priority
    pub priority: Option<u8>,
    /// Message TTL
    pub ttl: Option<Duration>,
    /// Publisher ID
    pub publisher_id: String,
    /// Message metadata
    pub metadata: HashMap<String, Value>,
}

/// PubSub execution context
#[derive(Debug)]
pub struct PubSubExecutionContext {
    /// PubSub configuration
    pub config: PubSubConfig,
    /// Shared cache
    pub cache: Arc<Cache>,
    /// Resource tracker
    pub resource_tracker: Arc<ResourceTracker>,
    /// Channel registry
    pub channels: Arc<RwLock<HashMap<String, ChannelConfig>>>,
    /// Subscription registry
    pub subscriptions: Arc<RwLock<HashMap<String, SubscriptionConfig>>>,
    /// Message store
    pub message_store: Arc<RwLock<HashMap<String, Vec<Message>>>>,
    /// Shutdown signal
    pub shutdown_signal: Arc<tokio::sync::Notify>,
}

/// Fluent builder for PubSub configuration
#[derive(Debug)]
pub struct PubSubBuilder {
    config: PubSubConfig,
    channels: HashMap<String, ChannelConfig>,
    subscriptions: HashMap<String, SubscriptionConfig>,
}

impl Default for PubSubBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl PubSubBuilder {
    /// Create a new PubSub builder
    pub fn new() -> Self {
        Self {
            config: PubSubConfig::default(),
            channels: HashMap::new(),
            subscriptions: HashMap::new(),
        }
    }

    /// Create a builder with a specific name
    pub fn named<S: Into<String>>(name: S) -> Self {
        Self {
            config: PubSubConfig {
                name: name.into(),
                ..Default::default()
            },
            channels: HashMap::new(),
            subscriptions: HashMap::new(),
        }
    }

    /// Set system name
    pub fn name<S: Into<String>>(mut self, name: S) -> Self {
        self.config.name = name.into();
        self
    }

    /// Set system description
    pub fn description<S: Into<String>>(mut self, description: S) -> Self {
        self.config.description = Some(description.into());
        self
    }

    /// Set maximum number of channels
    pub fn max_channels(mut self, count: usize) -> Self {
        self.config.max_channels = count;
        self
    }

    /// Set maximum subscribers per channel
    pub fn max_subscribers_per_channel(mut self, count: usize) -> Self {
        self.config.max_subscribers_per_channel = count;
        self
    }

    /// Set maximum message size
    pub fn max_message_size_bytes(mut self, bytes: usize) -> Self {
        self.config.max_message_size_bytes = bytes;
        self
    }

    /// Set maximum message size in KB
    pub fn max_message_size_kb(mut self, kb: usize) -> Self {
        self.config.max_message_size_bytes = kb * 1024;
        self
    }

    /// Set maximum message size in MB
    pub fn max_message_size_mb(mut self, mb: usize) -> Self {
        self.config.max_message_size_bytes = mb * 1024 * 1024;
        self
    }

    /// Configure retention policy
    pub fn retention_policy<F>(mut self, configure: F) -> Self
    where
        F: FnOnce(RetentionPolicyBuilder) -> RetentionPolicyBuilder,
    {
        let builder = RetentionPolicyBuilder::new(self.config.retention_policy);
        self.config.retention_policy = configure(builder).build();
        self
    }

    /// Set delivery guarantees
    pub fn delivery_guarantees(mut self, guarantees: DeliveryGuarantees) -> Self {
        self.config.delivery_guarantees = guarantees;
        self
    }

    /// Set message ordering
    pub fn ordering(mut self, ordering: MessageOrdering) -> Self {
        self.config.ordering = ordering;
        self
    }

    /// Configure performance settings
    pub fn performance<F>(mut self, configure: F) -> Self
    where
        F: FnOnce(PubSubPerformanceConfigBuilder) -> PubSubPerformanceConfigBuilder,
    {
        let builder = PubSubPerformanceConfigBuilder::new(self.config.performance);
        self.config.performance = configure(builder).build();
        self
    }

    /// Configure security settings
    pub fn security<F>(mut self, configure: F) -> Self
    where
        F: FnOnce(PubSubSecurityConfigBuilder) -> PubSubSecurityConfigBuilder,
    {
        let builder = PubSubSecurityConfigBuilder::new(self.config.security);
        self.config.security = configure(builder).build();
        self
    }

    /// Add a channel
    pub fn channel<F>(mut self, configure: F) -> Self
    where
        F: FnOnce(ChannelBuilder) -> ChannelBuilder,
    {
        let builder = ChannelBuilder::new();
        let channel = configure(builder).build();
        self.channels.insert(channel.name.clone(), channel);
        self
    }

    /// Add multiple channels
    pub fn channels<I, F>(mut self, channels: I) -> Self
    where
        I: IntoIterator<Item = F>,
        F: FnOnce(ChannelBuilder) -> ChannelBuilder,
    {
        for configure in channels {
            let builder = ChannelBuilder::new();
            let channel = configure(builder).build();
            self.channels.insert(channel.name.clone(), channel);
        }
        self
    }

    /// Add a subscription
    pub fn subscription<F>(mut self, configure: F) -> Self
    where
        F: FnOnce(SubscriptionBuilder) -> SubscriptionBuilder,
    {
        let builder = SubscriptionBuilder::new();
        let subscription = configure(builder).build();
        self.subscriptions
            .insert(subscription.name.clone(), subscription);
        self
    }

    /// Add metadata
    pub fn metadata<K, V>(mut self, key: K, value: V) -> Self
    where
        K: Into<String>,
        V: Serialize,
    {
        if let Ok(json_value) = serde_json::to_value(value) {
            self.config.metadata.insert(key.into(), json_value);
        }
        self
    }

    /// Build the configuration
    pub fn build(
        self,
    ) -> Result<(
        PubSubConfig,
        HashMap<String, ChannelConfig>,
        HashMap<String, SubscriptionConfig>,
    )> {
        self.validate()?;
        Ok((self.config, self.channels, self.subscriptions))
    }

    /// Validate the configuration
    fn validate(&self) -> Result<()> {
        if self.config.name.is_empty() {
            return Err(DaggerError::configuration("PubSub name cannot be empty"));
        }

        if self.config.max_channels == 0 {
            return Err(DaggerError::configuration(
                "max_channels must be greater than 0",
            ));
        }

        if self.config.max_subscribers_per_channel == 0 {
            return Err(DaggerError::configuration(
                "max_subscribers_per_channel must be greater than 0",
            ));
        }

        if self.config.max_message_size_bytes == 0 {
            return Err(DaggerError::configuration(
                "max_message_size_bytes must be greater than 0",
            ));
        }

        // Validate subscriptions reference existing channels
        for subscription in self.subscriptions.values() {
            if !self.channels.contains_key(&subscription.channel) {
                return Err(DaggerError::validation(format!(
                    "Subscription '{}' references non-existent channel '{}'",
                    subscription.name, subscription.channel
                )));
            }
        }

        Ok(())
    }

    /// Build execution context
    pub fn build_execution_context(
        self,
        cache: Arc<Cache>,
        resource_tracker: Arc<ResourceTracker>,
    ) -> Result<PubSubExecutionContext> {
        let (config, channels, subscriptions) = self.build()?;
        let channels = Arc::new(RwLock::new(channels));
        let subscriptions = Arc::new(RwLock::new(subscriptions));
        let message_store = Arc::new(RwLock::new(HashMap::new()));
        let shutdown_signal = Arc::new(tokio::sync::Notify::new());

        Ok(PubSubExecutionContext {
            config,
            cache,
            resource_tracker,
            channels,
            subscriptions,
            message_store,
            shutdown_signal,
        })
    }
}

/// Builder for retention policy
#[derive(Debug)]
pub struct RetentionPolicyBuilder {
    policy: MessageRetentionPolicy,
}

impl RetentionPolicyBuilder {
    fn new(policy: MessageRetentionPolicy) -> Self {
        Self { policy }
    }

    pub fn max_messages(mut self, count: usize) -> Self {
        self.policy.max_messages = count;
        self
    }

    pub fn max_age(mut self, duration: Duration) -> Self {
        self.policy.max_age = duration;
        self
    }

    pub fn max_storage_bytes(mut self, bytes: u64) -> Self {
        self.policy.max_storage_bytes = bytes;
        self
    }

    pub fn max_storage_mb(mut self, mb: u64) -> Self {
        self.policy.max_storage_bytes = mb * 1024 * 1024;
        self
    }

    pub fn cleanup_strategy(mut self, strategy: CleanupStrategy) -> Self {
        self.policy.cleanup_strategy = strategy;
        self
    }

    fn build(self) -> MessageRetentionPolicy {
        self.policy
    }
}

/// Builder for PubSub performance configuration
#[derive(Debug)]
pub struct PubSubPerformanceConfigBuilder {
    config: PubSubPerformanceConfig,
}

impl PubSubPerformanceConfigBuilder {
    fn new(config: PubSubPerformanceConfig) -> Self {
        Self { config }
    }

    pub fn channel_buffer_size(mut self, size: usize) -> Self {
        self.config.channel_buffer_size = size;
        self
    }

    pub fn batch_size(mut self, size: usize) -> Self {
        self.config.batch_size = size;
        self
    }

    pub fn flush_interval(mut self, interval: Duration) -> Self {
        self.config.flush_interval = interval;
        self
    }

    pub fn worker_threads(mut self, threads: usize) -> Self {
        self.config.worker_threads = threads;
        self
    }

    pub fn enable_compression(mut self, enabled: bool) -> Self {
        self.config.enable_compression = enabled;
        self
    }

    pub fn compression_threshold(mut self, bytes: usize) -> Self {
        self.config.compression_threshold = bytes;
        self
    }

    pub fn async_persistence(mut self, enabled: bool) -> Self {
        self.config.async_persistence = enabled;
        self
    }

    fn build(self) -> PubSubPerformanceConfig {
        self.config
    }
}

/// Builder for PubSub security configuration
#[derive(Debug)]
pub struct PubSubSecurityConfigBuilder {
    config: PubSubSecurityConfig,
}

impl PubSubSecurityConfigBuilder {
    fn new(config: PubSubSecurityConfig) -> Self {
        Self { config }
    }

    pub fn enable_publisher_auth(mut self, enabled: bool) -> Self {
        self.config.enable_publisher_auth = enabled;
        self
    }

    pub fn enable_subscriber_auth(mut self, enabled: bool) -> Self {
        self.config.enable_subscriber_auth = enabled;
        self
    }

    pub fn enable_encryption(mut self, enabled: bool) -> Self {
        self.config.enable_encryption = enabled;
        self
    }

    pub fn enable_acl(mut self, enabled: bool) -> Self {
        self.config.enable_acl = enabled;
        self
    }

    pub fn message_validation<S: Into<String>>(mut self, schema: S) -> Self {
        self.config.message_validation = Some(schema.into());
        self
    }

    pub fn rate_limiting<F>(mut self, configure: F) -> Self
    where
        F: FnOnce(RateLimitConfigBuilder) -> RateLimitConfigBuilder,
    {
        let builder = RateLimitConfigBuilder::new();
        self.config.rate_limiting = Some(configure(builder).build());
        self
    }

    fn build(self) -> PubSubSecurityConfig {
        self.config
    }
}

/// Builder for rate limit configuration
#[derive(Debug)]
pub struct RateLimitConfigBuilder {
    config: RateLimitConfig,
}

impl RateLimitConfigBuilder {
    fn new() -> Self {
        Self {
            config: RateLimitConfig {
                max_messages_per_sec: 1000,
                max_bytes_per_sec: 1024 * 1024, // 1MB/s
                window_size: Duration::from_secs(1),
                action: RateLimitAction::Queue,
            },
        }
    }

    pub fn max_messages_per_sec(mut self, count: u32) -> Self {
        self.config.max_messages_per_sec = count;
        self
    }

    pub fn max_bytes_per_sec(mut self, bytes: u64) -> Self {
        self.config.max_bytes_per_sec = bytes;
        self
    }

    pub fn window_size(mut self, duration: Duration) -> Self {
        self.config.window_size = duration;
        self
    }

    pub fn action(mut self, action: RateLimitAction) -> Self {
        self.config.action = action;
        self
    }

    fn build(self) -> RateLimitConfig {
        self.config
    }
}

/// Builder for channel configuration
#[derive(Debug)]
pub struct ChannelBuilder {
    config: ChannelConfig,
}

impl ChannelBuilder {
    fn new() -> Self {
        Self {
            config: ChannelConfig::new(format!("channel_{}", Uuid::new_v4())),
        }
    }

    pub fn name<S: Into<String>>(mut self, name: S) -> Self {
        self.config.name = name.into();
        self
    }

    pub fn description<S: Into<String>>(mut self, description: S) -> Self {
        self.config.description = Some(description.into());
        self
    }

    pub fn persistent(mut self, persistent: bool) -> Self {
        self.config.persistent = persistent;
        self
    }

    pub fn retention_policy<F>(mut self, configure: F) -> Self
    where
        F: FnOnce(RetentionPolicyBuilder) -> RetentionPolicyBuilder,
    {
        let builder = RetentionPolicyBuilder::new(MessageRetentionPolicy::default());
        self.config.retention_policy = Some(configure(builder).build());
        self
    }

    pub fn delivery_guarantees(mut self, guarantees: DeliveryGuarantees) -> Self {
        self.config.delivery_guarantees = Some(guarantees);
        self
    }

    pub fn message_schema<T: Serialize>(mut self, schema: T) -> Self {
        if let Ok(value) = serde_json::to_value(schema) {
            self.config.message_schema = Some(value);
        }
        self
    }

    pub fn metadata<K, V>(mut self, key: K, value: V) -> Self
    where
        K: Into<String>,
        V: Serialize,
    {
        if let Ok(json_value) = serde_json::to_value(value) {
            self.config.metadata.insert(key.into(), json_value);
        }
        self
    }

    fn build(self) -> ChannelConfig {
        self.config
    }
}

/// Builder for subscription configuration
#[derive(Debug)]
pub struct SubscriptionBuilder {
    config: SubscriptionConfig,
}

impl SubscriptionBuilder {
    fn new() -> Self {
        Self {
            config: SubscriptionConfig {
                name: format!("subscription_{}", Uuid::new_v4()),
                channel: String::new(),
                filter: None,
                subscription_type: SubscriptionType::Shared,
                start_position: StartPosition::Latest,
                max_unacked_messages: 1000,
                ack_timeout: Duration::from_secs(30),
                metadata: HashMap::new(),
            },
        }
    }

    pub fn name<S: Into<String>>(mut self, name: S) -> Self {
        self.config.name = name.into();
        self
    }

    pub fn channel<S: Into<String>>(mut self, channel: S) -> Self {
        self.config.channel = channel.into();
        self
    }

    pub fn filter<S: Into<String>>(mut self, filter: S) -> Self {
        self.config.filter = Some(filter.into());
        self
    }

    pub fn subscription_type(mut self, sub_type: SubscriptionType) -> Self {
        self.config.subscription_type = sub_type;
        self
    }

    pub fn start_position(mut self, position: StartPosition) -> Self {
        self.config.start_position = position;
        self
    }

    pub fn max_unacked_messages(mut self, count: usize) -> Self {
        self.config.max_unacked_messages = count;
        self
    }

    pub fn ack_timeout(mut self, timeout: Duration) -> Self {
        self.config.ack_timeout = timeout;
        self
    }

    pub fn metadata<K, V>(mut self, key: K, value: V) -> Self
    where
        K: Into<String>,
        V: Serialize,
    {
        if let Ok(json_value) = serde_json::to_value(value) {
            self.config.metadata.insert(key.into(), json_value);
        }
        self
    }

    fn build(self) -> SubscriptionConfig {
        self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pubsub_builder() {
        let (config, channels, subscriptions) = PubSubBuilder::new()
            .name("test_pubsub")
            .description("A test PubSub system")
            .max_channels(500)
            .max_subscribers_per_channel(50)
            .max_message_size_mb(5)
            .delivery_guarantees(DeliveryGuarantees::ExactlyOnce)
            .ordering(MessageOrdering::Global)
            .retention_policy(|policy| {
                policy
                    .max_messages(5000)
                    .max_age(Duration::from_secs(3600))
                    .cleanup_strategy(CleanupStrategy::OldestFirst)
            })
            .performance(|perf| {
                perf.channel_buffer_size(2000)
                    .batch_size(50)
                    .worker_threads(8)
                    .enable_compression(true)
            })
            .security(|sec| {
                sec.enable_publisher_auth(true)
                    .enable_encryption(true)
                    .rate_limiting(|rate| {
                        rate.max_messages_per_sec(500)
                            .action(RateLimitAction::Queue)
                    })
            })
            .channel(|ch| {
                ch.name("events")
                    .description("Event channel")
                    .persistent(true)
                    .delivery_guarantees(DeliveryGuarantees::AtLeastOnce)
            })
            .subscription(|sub| {
                sub.name("event_processor")
                    .channel("events")
                    .subscription_type(SubscriptionType::Shared)
                    .start_position(StartPosition::Earliest)
            })
            .build()
            .unwrap();

        assert_eq!(config.name, "test_pubsub");
        assert_eq!(config.max_channels, 500);
        assert_eq!(config.max_message_size_bytes, 5 * 1024 * 1024);
        assert!(matches!(
            config.delivery_guarantees,
            DeliveryGuarantees::ExactlyOnce
        ));
        assert!(channels.contains_key("events"));
        assert!(subscriptions.contains_key("event_processor"));
        assert_eq!(subscriptions["event_processor"].channel, "events");
    }

    #[test]
    fn test_validation() {
        // Test subscription referencing non-existent channel
        let result = PubSubBuilder::new()
            .subscription(|sub| sub.name("test_sub").channel("nonexistent"))
            .build();

        assert!(result.is_err());
    }
}
