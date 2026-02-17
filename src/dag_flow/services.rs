//! Typed service registry for embedding Dagger in host applications.
//!
//! `DagExecutor::set_services(...)` stores a single `Arc<dyn Any>`. For Work Queue primitives we
//! often want multiple independent services (exec policy, HITL runtime, etc). This registry is a
//! lightweight container the host can store as that single `services` value, while nodes can still
//! access typed services via downcasting.

use parking_lot::RwLock;
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Default, Clone)]
pub struct ServiceRegistry {
    inner: Arc<RwLock<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>>,
}

impl ServiceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert<T: Any + Send + Sync + 'static>(&self, service: Arc<T>) {
        let mut inner = self.inner.write();
        inner.insert(TypeId::of::<T>(), service as Arc<dyn Any + Send + Sync>);
    }

    pub fn get<T: Any + Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        let inner = self.inner.read();
        inner
            .get(&TypeId::of::<T>())
            .cloned()
            .and_then(|svc| svc.downcast::<T>().ok())
    }

    pub fn with<T: Any + Send + Sync + 'static>(self, service: Arc<T>) -> Self {
        self.insert(service);
        self
    }
}
