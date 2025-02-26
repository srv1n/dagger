use anyhow::Result;
use dagger::{get_global_input,insert_global_value, Cache};
use serde::{Serialize, Deserialize};
use serde_json::Value;
use std::collections::HashMap;
use anyhow::anyhow;

pub fn update_global_task_queue(cache: &Cache, updated_queue: Vec<Value>) -> Result<()> {
    insert_global_value(cache, "global", "task_queue", updated_queue)?;
    Ok(())
}

pub fn get_pending_tasks(cache: &Cache) -> Result<Vec<Value>> {
    let task_queue: Vec<Value> = get_global_input(cache, "global", "task_queue")?;
    Ok(task_queue.into_iter().filter(|t| t["status"].as_str() == Some("pending")).collect())
}