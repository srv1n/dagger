use super::taskagent::TaskAgent;
use anyhow::Result;
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

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
        let factory = agents
            .get(name)
            .ok_or(anyhow::anyhow!("Agent {} not found", name))?;
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

#[derive(Default)]
pub struct TypeRegistry {
    types: RwLock<HashMap<TypeId, Box<dyn Any + Send + Sync>>>,
}

impl TypeRegistry {
    pub fn new() -> Self {
        Self {
            types: RwLock::new(HashMap::new()),
        }
    }

    pub async fn register<T: 'static + Send + Sync>(&self, instance: T) -> Result<()> {
        let mut types = self.types.write().await;
        types.insert(TypeId::of::<T>(), Box::new(instance));
        Ok(())
    }

    pub async fn get<T: 'static + Clone + Send + Sync>(&self) -> Option<T> {
        let types = self.types.read().await;
        types
            .get(&TypeId::of::<T>())
            .and_then(|boxed| boxed.downcast_ref::<T>())
            .cloned()
    }
}
