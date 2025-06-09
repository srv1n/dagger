use crate::core::errors::{DaggerError, Result};
use crate::core::performance::{PerformanceMonitor, OperationMetrics};
use crate::core::limits::{ResourceTracker, ResourceUsageStats, ResourceUtilization};
use crate::core::memory::{Cache as ImprovedCache, CacheStats};
use crate::core::concurrency::{ConcurrentTaskRegistry, TaskNotificationSystem};
use crate::dag_flow::dag_builder::DagExecutionState;
use crate::taskagent::taskagent_builder::AgentState;
// use crate::pubsub::enhanced_pubsub::ChannelStats;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tokio::time::interval;
use tracing::{debug, info, warn};

/// Comprehensive system monitoring dashboard
#[derive(Debug)]
pub struct MonitoringDashboard {
    /// Performance monitor
    performance_monitor: Arc<PerformanceMonitor>,
    /// Resource tracker
    resource_tracker: Arc<ResourceTracker>,
    /// Cache reference
    cache: Arc<ImprovedCache>,
    /// Task registry
    task_registry: Arc<ConcurrentTaskRegistry>,
    /// Update interval
    update_interval: Duration,
    /// Historical metrics
    historical_metrics: Arc<RwLock<Vec<SystemSnapshot>>>,
    /// Maximum history size
    max_history: usize,
}

/// Complete system snapshot at a point in time
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemSnapshot {
    /// Timestamp of the snapshot
    pub timestamp: std::time::SystemTime,
    /// Resource usage statistics
    pub resource_usage: ResourceUsageStats,
    /// Resource utilization percentages
    pub resource_utilization: ResourceUtilization,
    /// Cache statistics
    pub cache_stats: CacheStats,
    /// Performance metrics by operation
    pub performance_metrics: HashMap<String, OperationMetrics>,
    /// Task registry statistics
    pub task_stats: TaskRegistryStats,
    /// System health status
    pub health_status: SystemHealth,
    /// Alerts and warnings
    pub alerts: Vec<Alert>,
}

/// Task registry statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRegistryStats {
    pub total_tasks: u64,
    pub tasks_by_status: HashMap<String, usize>,
    pub notifications_sent: u64,
    pub available_permits: usize,
}

/// System health status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemHealth {
    pub overall_status: HealthStatus,
    pub memory_health: HealthStatus,
    pub cpu_health: HealthStatus,
    pub task_health: HealthStatus,
    pub cache_health: HealthStatus,
    pub performance_health: HealthStatus,
}

/// Health status levels
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum HealthStatus {
    Healthy,
    Warning,
    Critical,
    Unknown,
}

/// System alert
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Alert {
    pub level: AlertLevel,
    pub component: String,
    pub message: String,
    pub timestamp: Instant,
    pub details: Option<HashMap<String, String>>,
}

/// Alert severity levels
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, PartialOrd)]
pub enum AlertLevel {
    Info,
    Warning,
    Error,
    Critical,
}

impl MonitoringDashboard {
    /// Create a new monitoring dashboard
    pub fn new(
        performance_monitor: Arc<PerformanceMonitor>,
        resource_tracker: Arc<ResourceTracker>,
        cache: Arc<ImprovedCache>,
        task_registry: Arc<ConcurrentTaskRegistry>,
        update_interval: Duration,
    ) -> Self {
        Self {
            performance_monitor,
            resource_tracker,
            cache,
            task_registry,
            update_interval,
            historical_metrics: Arc::new(RwLock::new(Vec::new())),
            max_history: 1000,
        }
    }
    
    /// Start monitoring with automatic updates
    pub async fn start_monitoring(&self) {
        let dashboard = self.clone();
        
        tokio::spawn(async move {
            let mut interval = interval(dashboard.update_interval);
            
            loop {
                interval.tick().await;
                
                match dashboard.collect_metrics().await {
                    Ok(snapshot) => {
                        dashboard.store_snapshot(snapshot).await;
                    }
                    Err(e) => {
                        warn!("Failed to collect metrics: {}", e);
                    }
                }
            }
        });
        
        info!("Monitoring dashboard started with {:?} update interval", self.update_interval);
    }
    
    /// Collect current system metrics
    pub async fn collect_metrics(&self) -> Result<SystemSnapshot> {
        let timestamp = std::time::SystemTime::now();
        let mut alerts = Vec::new();
        
        // Collect resource statistics
        let resource_usage = self.resource_tracker.get_usage_stats();
        let resource_utilization = self.resource_tracker.get_utilization();
        
        // Check resource alerts
        if resource_utilization.memory_utilization > 90.0 {
            alerts.push(Alert {
                level: AlertLevel::Critical,
                component: "memory".to_string(),
                message: format!("Memory usage critical: {:.1}%", resource_utilization.memory_utilization),
                timestamp,
                details: None,
            });
        } else if resource_utilization.memory_utilization > 80.0 {
            alerts.push(Alert {
                level: AlertLevel::Warning,
                component: "memory".to_string(),
                message: format!("Memory usage high: {:.1}%", resource_utilization.memory_utilization),
                timestamp,
                details: None,
            });
        }
        
        // Collect cache statistics
        let cache_stats = self.cache.get_stats().await;
        
        // Check cache health
        if cache_stats.hit_ratio() < 0.5 && cache_stats.hit_count + cache_stats.miss_count > 100 {
            alerts.push(Alert {
                level: AlertLevel::Warning,
                component: "cache".to_string(),
                message: format!("Low cache hit ratio: {:.1}%", cache_stats.hit_ratio() * 100.0),
                timestamp,
                details: None,
            });
        }
        
        // Collect performance metrics
        let performance_metrics: HashMap<String, OperationMetrics> = self.performance_monitor
            .get_all_metrics()
            .into_iter()
            .map(|m| (m.operation.clone(), m))
            .collect();
        
        // Check for slow operations
        for (op_name, metrics) in &performance_metrics {
            if metrics.p95_duration > Duration::from_secs(5) {
                alerts.push(Alert {
                    level: AlertLevel::Warning,
                    component: "performance".to_string(),
                    message: format!("Slow operation '{}': p95={:?}", op_name, metrics.p95_duration),
                    timestamp,
                    details: None,
                });
            }
            
            if metrics.error_rate() > 0.1 {
                alerts.push(Alert {
                    level: AlertLevel::Error,
                    component: "performance".to_string(),
                    message: format!("High error rate for '{}': {:.1}%", op_name, metrics.error_rate() * 100.0),
                    timestamp,
                    details: None,
                });
            }
        }
        
        // Collect task statistics
        let task_stats = TaskRegistryStats {
            total_tasks: self.task_registry.task_count(),
            tasks_by_status: self.task_registry.get_status_counts()
                .into_iter()
                .map(|(status, count)| (format!("{:?}", status), count))
                .collect(),
            notifications_sent: self.task_registry.get_notification_system().get_notifications_sent(),
            available_permits: self.task_registry.get_notification_system().available_permits(),
        };
        
        // Determine overall health
        let health_status = self.calculate_health_status(
            &resource_utilization,
            &cache_stats,
            &performance_metrics,
            &task_stats,
        );
        
        Ok(SystemSnapshot {
            timestamp,
            resource_usage,
            resource_utilization,
            cache_stats,
            performance_metrics,
            task_stats,
            health_status,
            alerts,
        })
    }
    
    /// Calculate system health status
    fn calculate_health_status(
        &self,
        resource_util: &ResourceUtilization,
        cache_stats: &CacheStats,
        perf_metrics: &HashMap<String, OperationMetrics>,
        task_stats: &TaskRegistryStats,
    ) -> SystemHealth {
        // Memory health
        let memory_health = if resource_util.memory_utilization > 90.0 {
            HealthStatus::Critical
        } else if resource_util.memory_utilization > 80.0 {
            HealthStatus::Warning
        } else {
            HealthStatus::Healthy
        };
        
        // CPU health (based on task utilization as proxy)
        let cpu_health = if resource_util.task_utilization > 90.0 {
            HealthStatus::Critical
        } else if resource_util.task_utilization > 80.0 {
            HealthStatus::Warning
        } else {
            HealthStatus::Healthy
        };
        
        // Task health
        let failed_tasks = task_stats.tasks_by_status.get("Failed").unwrap_or(&0);
        let total_completed = task_stats.tasks_by_status.get("Completed").unwrap_or(&0);
        let failure_rate = if total_completed + failed_tasks > 0 {
            *failed_tasks as f64 / (*total_completed + failed_tasks) as f64
        } else {
            0.0
        };
        
        let task_health = if failure_rate > 0.2 {
            HealthStatus::Critical
        } else if failure_rate > 0.1 {
            HealthStatus::Warning
        } else {
            HealthStatus::Healthy
        };
        
        // Cache health
        let cache_health = if cache_stats.hit_ratio() < 0.3 {
            HealthStatus::Critical
        } else if cache_stats.hit_ratio() < 0.5 {
            HealthStatus::Warning
        } else {
            HealthStatus::Healthy
        };
        
        // Performance health
        let avg_error_rate = if !perf_metrics.is_empty() {
            perf_metrics.values().map(|m| m.error_rate()).sum::<f64>() / perf_metrics.len() as f64
        } else {
            0.0
        };
        
        let performance_health = if avg_error_rate > 0.2 {
            HealthStatus::Critical
        } else if avg_error_rate > 0.1 {
            HealthStatus::Warning
        } else {
            HealthStatus::Healthy
        };
        
        // Overall health (worst of all components)
        let overall_status = vec![
            &memory_health,
            &cpu_health,
            &task_health,
            &cache_health,
            &performance_health,
        ]
        .into_iter()
        .max_by_key(|h| match h {
            HealthStatus::Critical => 3,
            HealthStatus::Warning => 2,
            HealthStatus::Healthy => 1,
            HealthStatus::Unknown => 0,
        })
        .cloned()
        .unwrap_or(HealthStatus::Unknown);
        
        SystemHealth {
            overall_status,
            memory_health,
            cpu_health,
            task_health,
            cache_health,
            performance_health,
        }
    }
    
    /// Store snapshot in history
    async fn store_snapshot(&self, snapshot: SystemSnapshot) {
        let mut history = self.historical_metrics.write().await;
        
        history.push(snapshot);
        
        // Trim history if needed
        if history.len() > self.max_history {
            let drain_count = history.len() - self.max_history;
            history.drain(0..drain_count);
        }
    }
    
    /// Get current system status
    pub async fn get_current_status(&self) -> Result<SystemSnapshot> {
        self.collect_metrics().await
    }
    
    /// Get historical metrics
    pub async fn get_history(&self, duration: Duration) -> Vec<SystemSnapshot> {
        let history = self.historical_metrics.read().await;
        let cutoff = std::time::SystemTime::now() - duration;
        
        history
            .iter()
            .filter(|s| s.timestamp > cutoff)
            .cloned()
            .collect()
    }
    
    /// Get system report
    pub async fn generate_report(&self) -> SystemReport {
        let current = self.collect_metrics().await.unwrap_or_else(|_| SystemSnapshot {
            timestamp: std::time::SystemTime::now(),
            resource_usage: ResourceUsageStats {
                memory_usage: 0,
                concurrent_tasks: 0,
                execution_tree_nodes: 0,
                cache_entries: 0,
                file_handles: 0,
                network_connections: 0,
                peak_memory_usage: 0,
                peak_concurrent_tasks: 0,
                total_tasks_executed: 0,
                total_limit_violations: 0,
            },
            resource_utilization: ResourceUtilization {
                memory_utilization: 0.0,
                task_utilization: 0.0,
                tree_utilization: 0.0,
                cache_utilization: 0.0,
                file_utilization: 0.0,
                network_utilization: 0.0,
            },
            cache_stats: CacheStats {
                total_entries: 0,
                total_size_bytes: 0,
                hit_count: 0,
                miss_count: 0,
                eviction_count: 0,
                cleanup_count: 0,
            },
            performance_metrics: HashMap::new(),
            task_stats: TaskRegistryStats {
                total_tasks: 0,
                tasks_by_status: HashMap::new(),
                notifications_sent: 0,
                available_permits: 0,
            },
            health_status: SystemHealth {
                overall_status: HealthStatus::Unknown,
                memory_health: HealthStatus::Unknown,
                cpu_health: HealthStatus::Unknown,
                task_health: HealthStatus::Unknown,
                cache_health: HealthStatus::Unknown,
                performance_health: HealthStatus::Unknown,
            },
            alerts: Vec::new(),
        });
        
        let history = self.get_history(Duration::from_secs(3600)).await; // Last hour
        
        let summary = self.generate_summary(&current).await;
        let recommendations = self.generate_recommendations(&current);
        
        SystemReport {
            generated_at: Instant::now(),
            current_snapshot: current,
            historical_snapshots: history,
            summary,
            recommendations,
        }
    }
    
    /// Generate summary statistics
    async fn generate_summary(&self, current: &SystemSnapshot) -> SystemSummary {
        let history = self.get_history(Duration::from_secs(3600)).await;
        
        // Calculate averages from history
        let avg_memory_usage = if !history.is_empty() {
            history.iter().map(|s| s.resource_usage.memory_usage).sum::<u64>() / history.len() as u64
        } else {
            current.resource_usage.memory_usage
        };
        
        let avg_cache_hit_ratio = if !history.is_empty() {
            history.iter().map(|s| s.cache_stats.hit_ratio()).sum::<f64>() / history.len() as f64
        } else {
            current.cache_stats.hit_ratio()
        };
        
        SystemSummary {
            total_tasks_executed: current.resource_usage.total_tasks_executed,
            average_memory_usage: avg_memory_usage,
            peak_memory_usage: current.resource_usage.peak_memory_usage,
            average_cache_hit_ratio: avg_cache_hit_ratio,
            total_errors: current.performance_metrics.values()
                .map(|m| m.error_count)
                .sum(),
            alert_count: current.alerts.len(),
            critical_alerts: current.alerts.iter()
                .filter(|a| a.level == AlertLevel::Critical)
                .count(),
        }
    }
    
    /// Generate recommendations based on current state
    fn generate_recommendations(&self, current: &SystemSnapshot) -> Vec<String> {
        let mut recommendations = Vec::new();
        
        // Memory recommendations
        if current.resource_utilization.memory_utilization > 80.0 {
            recommendations.push(
                "Consider increasing memory limits or optimizing memory usage".to_string()
            );
        }
        
        // Cache recommendations
        if current.cache_stats.hit_ratio() < 0.5 {
            recommendations.push(
                "Low cache hit ratio detected. Consider reviewing cache configuration or access patterns".to_string()
            );
        }
        
        // Performance recommendations
        for (op, metrics) in &current.performance_metrics {
            if metrics.error_rate() > 0.1 {
                recommendations.push(format!(
                    "High error rate for operation '{}'. Review error logs and retry policies", op
                ));
            }
            
            if metrics.p95_duration > Duration::from_secs(10) {
                recommendations.push(format!(
                    "Slow performance for operation '{}'. Consider optimization or timeout adjustment", op
                ));
            }
        }
        
        // Task recommendations
        if current.task_stats.available_permits == 0 {
            recommendations.push(
                "Task execution at capacity. Consider increasing concurrent task limits".to_string()
            );
        }
        
        recommendations
    }
}

// Allow cloning for concurrent access
impl Clone for MonitoringDashboard {
    fn clone(&self) -> Self {
        Self {
            performance_monitor: Arc::clone(&self.performance_monitor),
            resource_tracker: Arc::clone(&self.resource_tracker),
            cache: Arc::clone(&self.cache),
            task_registry: Arc::clone(&self.task_registry),
            update_interval: self.update_interval,
            historical_metrics: Arc::clone(&self.historical_metrics),
            max_history: self.max_history,
        }
    }
}

/// Complete system report
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemReport {
    pub generated_at: Instant,
    pub current_snapshot: SystemSnapshot,
    pub historical_snapshots: Vec<SystemSnapshot>,
    pub summary: SystemSummary,
    pub recommendations: Vec<String>,
}

/// System summary statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemSummary {
    pub total_tasks_executed: u64,
    pub average_memory_usage: u64,
    pub peak_memory_usage: u64,
    pub average_cache_hit_ratio: f64,
    pub total_errors: u64,
    pub alert_count: usize,
    pub critical_alerts: usize,
}

/// Monitoring configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitoringConfig {
    /// Update interval for metrics collection
    pub update_interval: Duration,
    /// Maximum history size
    pub max_history_size: usize,
    /// Enable automatic alerting
    pub enable_alerts: bool,
    /// Alert thresholds
    pub alert_thresholds: AlertThresholds,
}

impl Default for MonitoringConfig {
    fn default() -> Self {
        Self {
            update_interval: Duration::from_secs(10),
            max_history_size: 1000,
            enable_alerts: true,
            alert_thresholds: AlertThresholds::default(),
        }
    }
}

/// Alert threshold configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertThresholds {
    pub memory_warning_percent: f64,
    pub memory_critical_percent: f64,
    pub error_rate_warning: f64,
    pub error_rate_critical: f64,
    pub cache_hit_ratio_warning: f64,
    pub cache_hit_ratio_critical: f64,
    pub task_failure_rate_warning: f64,
    pub task_failure_rate_critical: f64,
}

impl Default for AlertThresholds {
    fn default() -> Self {
        Self {
            memory_warning_percent: 80.0,
            memory_critical_percent: 90.0,
            error_rate_warning: 0.1,
            error_rate_critical: 0.2,
            cache_hit_ratio_warning: 0.5,
            cache_hit_ratio_critical: 0.3,
            task_failure_rate_warning: 0.1,
            task_failure_rate_critical: 0.2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builders::DaggerBuilder;
    use tokio::test;
    
    #[test]
    async fn test_monitoring_dashboard() {
        // Create configuration
        let dagger_config = DaggerBuilder::testing().build().unwrap();
        
        // Create shared resources
        let cache = Arc::new(ImprovedCache::new(dagger_config.cache.clone()).unwrap());
        let resource_tracker = Arc::new(ResourceTracker::new(dagger_config.limits.clone()).unwrap());
        let task_registry = Arc::new(ConcurrentTaskRegistry::new(10));
        let performance_monitor = Arc::new(PerformanceMonitor::new(100));
        
        // Create monitoring dashboard
        let dashboard = MonitoringDashboard::new(
            performance_monitor.clone(),
            resource_tracker,
            cache,
            task_registry,
            Duration::from_secs(1),
        );
        
        // Start monitoring
        dashboard.start_monitoring().await;
        
        // Record some test metrics
        performance_monitor.record_operation("test_op", Duration::from_millis(100));
        performance_monitor.record_operation("test_op", Duration::from_millis(200));
        performance_monitor.record_error("test_op");
        
        // Wait for collection
        tokio::time::sleep(Duration::from_secs(2)).await;
        
        // Get current status
        let status = dashboard.get_current_status().await.unwrap();
        assert!(status.performance_metrics.contains_key("test_op"));
        
        // Generate report
        let report = dashboard.generate_report().await;
        assert!(!report.recommendations.is_empty());
    }
}