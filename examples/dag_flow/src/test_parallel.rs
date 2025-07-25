use anyhow::Result;
use dagger::{
    insert_value, DagConfig, DagExecutor, serialize_cache_to_prettyjson,
};
use dagger::dag_flow::Cache;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tokio::sync::oneshot;
use tracing::Level;
use tracing_subscriber::FmtSubscriber;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;

    println!("Creating executor with parallel execution enabled...");
    
    // Create config with parallel execution enabled
    let mut config = DagConfig::default();
    config.enable_parallel_execution = true;
    config.max_parallel_nodes = 4;
    
    let registry = Arc::new(RwLock::new(HashMap::new()));
    let executor = DagExecutor::new(Some(config), registry.clone(), "test_db")?;
    
    println!("Executor created successfully!");
    println!("Parallel execution enabled: {}", executor.config.enable_parallel_execution);
    println!("Max parallel nodes: {}", executor.config.max_parallel_nodes);
    
    // Create a simple cache
    let cache = Cache::new();
    insert_value(&cache, "test", "value", 42)?;
    
    let json = serialize_cache_to_prettyjson(&cache)?;
    println!("Cache contents:\n{}", json);
    
    Ok(())
}