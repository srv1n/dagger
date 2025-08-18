//! Task metrics collection and reporting module
//!
//! This module provides optional metrics collection for task execution,
//! performance monitoring, and system health tracking.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Task execution metrics
#[derive(Debug, Default)]
pub struct TaskMetrics {
    /// Total number of tasks submitted
    pub tasks_submitted: AtomicU64,
    /// Total number of tasks completed successfully
    pub tasks_completed: AtomicU64,
    /// Total number of tasks failed
    pub tasks_failed: AtomicU64,
    /// Total number of tasks cancelled
    pub tasks_cancelled: AtomicU64,
    /// Total execution time across all tasks
    pub total_execution_time_ms: AtomicU64,
    /// Average execution time per task
    pub avg_execution_time_ms: AtomicU64,
}

impl TaskMetrics {
    /// Create new task metrics
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a task submission
    pub fn record_task_submitted(&self) {
        self.tasks_submitted.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a task completion
    pub fn record_task_completed(&self, execution_time: Duration) {
        self.tasks_completed.fetch_add(1, Ordering::Relaxed);
        self.total_execution_time_ms
            .fetch_add(execution_time.as_millis() as u64, Ordering::Relaxed);

        // Update average (simple running average)
        let completed = self.tasks_completed.load(Ordering::Relaxed);
        let total_time = self.total_execution_time_ms.load(Ordering::Relaxed);
        if completed > 0 {
            self.avg_execution_time_ms
                .store(total_time / completed, Ordering::Relaxed);
        }
    }

    /// Record a task failure
    pub fn record_task_failed(&self) {
        self.tasks_failed.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a task cancellation
    pub fn record_task_cancelled(&self) {
        self.tasks_cancelled.fetch_add(1, Ordering::Relaxed);
    }

    /// Get current metrics snapshot
    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            tasks_submitted: self.tasks_submitted.load(Ordering::Relaxed),
            tasks_completed: self.tasks_completed.load(Ordering::Relaxed),
            tasks_failed: self.tasks_failed.load(Ordering::Relaxed),
            tasks_cancelled: self.tasks_cancelled.load(Ordering::Relaxed),
            total_execution_time_ms: self.total_execution_time_ms.load(Ordering::Relaxed),
            avg_execution_time_ms: self.avg_execution_time_ms.load(Ordering::Relaxed),
        }
    }

    /// Reset all metrics
    pub fn reset(&self) {
        self.tasks_submitted.store(0, Ordering::Relaxed);
        self.tasks_completed.store(0, Ordering::Relaxed);
        self.tasks_failed.store(0, Ordering::Relaxed);
        self.tasks_cancelled.store(0, Ordering::Relaxed);
        self.total_execution_time_ms.store(0, Ordering::Relaxed);
        self.avg_execution_time_ms.store(0, Ordering::Relaxed);
    }
}

/// Snapshot of metrics at a point in time
#[derive(Debug, Clone)]
pub struct MetricsSnapshot {
    pub tasks_submitted: u64,
    pub tasks_completed: u64,
    pub tasks_failed: u64,
    pub tasks_cancelled: u64,
    pub total_execution_time_ms: u64,
    pub avg_execution_time_ms: u64,
}

impl MetricsSnapshot {
    /// Calculate success rate (0.0 to 1.0)
    pub fn success_rate(&self) -> f64 {
        if self.tasks_submitted == 0 {
            0.0
        } else {
            self.tasks_completed as f64 / self.tasks_submitted as f64
        }
    }

    /// Calculate failure rate (0.0 to 1.0)
    pub fn failure_rate(&self) -> f64 {
        if self.tasks_submitted == 0 {
            0.0
        } else {
            self.tasks_failed as f64 / self.tasks_submitted as f64
        }
    }

    /// Get tasks per second (requires time window)
    pub fn tasks_per_second(&self, window_duration: Duration) -> f64 {
        if window_duration.as_secs() == 0 {
            0.0
        } else {
            self.tasks_completed as f64 / window_duration.as_secs_f64()
        }
    }
}

/// Timer for measuring execution duration
pub struct ExecutionTimer {
    start: Instant,
}

impl ExecutionTimer {
    /// Start a new timer
    pub fn start() -> Self {
        Self {
            start: Instant::now(),
        }
    }

    /// Get elapsed duration since timer started
    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }

    /// Stop timer and return duration
    pub fn stop(self) -> Duration {
        self.elapsed()
    }
}

/// Global metrics instance
static GLOBAL_METRICS: once_cell::sync::Lazy<Arc<TaskMetrics>> =
    once_cell::sync::Lazy::new(|| Arc::new(TaskMetrics::new()));

/// Get reference to global metrics
pub fn global_metrics() -> &'static Arc<TaskMetrics> {
    &GLOBAL_METRICS
}

/// Macro for timing code execution
#[macro_export]
macro_rules! time_execution {
    ($metrics:expr, $body:expr) => {{
        let timer = $crate::metrics::ExecutionTimer::start();
        let result = $body;
        let duration = timer.stop();
        $metrics.record_task_completed(duration);
        result
    }};
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_metrics_basic() {
        let metrics = TaskMetrics::new();

        metrics.record_task_submitted();
        metrics.record_task_completed(Duration::from_millis(100));
        metrics.record_task_failed();

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.tasks_submitted, 1);
        assert_eq!(snapshot.tasks_completed, 1);
        assert_eq!(snapshot.tasks_failed, 1);
        assert_eq!(snapshot.success_rate(), 1.0);
        assert_eq!(snapshot.failure_rate(), 1.0);
    }

    #[test]
    fn test_execution_timer() {
        let timer = ExecutionTimer::start();
        thread::sleep(Duration::from_millis(10));
        let duration = timer.stop();

        assert!(duration >= Duration::from_millis(10));
        assert!(duration < Duration::from_millis(100));
    }

    #[test]
    fn test_global_metrics() {
        let metrics = global_metrics();
        metrics.record_task_submitted();

        let snapshot = metrics.snapshot();
        assert!(snapshot.tasks_submitted >= 1);
    }
}
