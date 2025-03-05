use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use anyhow::Result;

use super::taskagent::TaskAgent;

#[derive(Clone)]
pub struct GlobalAgentRegistry {
    agents: Arc<RwLock<HashMap<String, Arc<dyn Fn() -> Arc<dyn TaskAgent> + Send + Sync>>>>,
}

impl GlobalAgentRegistry {
    pub fn new() -> Self {
        Self {
            agents: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn register(
        &self,
        name: String,
        factory: Arc<dyn Fn() -> Arc<dyn TaskAgent> + Send + Sync>,
    ) -> Result<()> {
        let mut agents = self.agents.write().await;
        agents.insert(name, factory);
        Ok(())
    }

    pub async fn create_agent(&self, name: &str) -> Result<Arc<dyn TaskAgent>> {
        let agents = self.agents.read().await;
        let factory = agents.get(name).ok_or(anyhow::anyhow!("Agent {} not found", name))?;
        Ok(factory())
    }

    pub async fn list_agents(&self) -> Vec<String> {
        let agents = self.agents.read().await;
        agents.keys().cloned().collect()
    }
}

lazy_static::lazy_static! {
    pub static ref GLOBAL_REGISTRY: GlobalAgentRegistry = GlobalAgentRegistry::new();
}