// Re-export the taskagent registry
pub use crate::taskagent::registry::*;

// Add a type registry for dependency injection
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use anyhow::Result;

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

    pub fn register<T: 'static + Send + Sync>(&self, instance: T) -> Result<()> {
        let mut types = self.types.write().unwrap();
        types.insert(TypeId::of::<T>(), Box::new(instance));
        Ok(())
    }

    pub fn get<T: 'static + Clone + Send + Sync>(&self) -> Option<T> {
        let types = self.types.read().unwrap();
        types.get(&TypeId::of::<T>())
            .and_then(|boxed| boxed.downcast_ref::<T>())
            .cloned()
    }
}

lazy_static::lazy_static! {
    pub static ref GLOBAL_REGISTRY: TypeRegistry = TypeRegistry::new();
} 