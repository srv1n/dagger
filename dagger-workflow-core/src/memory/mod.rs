//! In-memory object store and control-plane store.

use crate::artifact::{
    ArtifactMetadataConflict, FailedReadClass, FailedReadProof, ObjectReadError, ObjectStore,
    ObjectStoreError, VerifiedObject, VerifiedObjectRef,
};
use crate::engine::Clock;
use crate::ids::Digest;
use crate::scope::ExecutionScope;
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::Read;
use std::sync::{Arc, Mutex};

fn entropy() -> std::io::Result<[u8; 32]> {
    let mut bytes = [0_u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes)
}

/// Scope-confined content-addressed volatile object storage.
pub struct InMemoryObjectStore<C> {
    clock: Arc<C>,
    nonce: Vec<u8>,
    objects: Mutex<BTreeMap<(ExecutionScope, Digest), MemoryObject>>,
}

#[derive(Clone)]
struct MemoryObject {
    bytes: Vec<u8>,
    media_type: String,
    object_key: String,
}

impl<C: Clock> InMemoryObjectStore<C> {
    /// Creates an empty object store using the supplied store clock.
    pub fn new(clock: Arc<C>) -> Self {
        Self {
            clock,
            nonce: entropy().unwrap_or([0x5a; 32]).to_vec(),
            objects: Mutex::new(BTreeMap::new()),
        }
    }

    /// Test-only corruption hook that replaces committed bytes without metadata changes.
    pub fn corrupt_bytes(
        &self,
        scope: &ExecutionScope,
        object_digest: &Digest,
        bytes: Vec<u8>,
    ) -> bool {
        let mut objects = self.objects.lock().expect("object lock poisoned");
        if let Some(object) = objects.get_mut(&(scope.clone(), object_digest.clone())) {
            object.bytes = bytes;
            true
        } else {
            false
        }
    }

    fn proof(
        &self,
        scope: &ExecutionScope,
        requested: &Digest,
        class: FailedReadClass,
        observed: Option<Digest>,
    ) -> FailedReadProof {
        FailedReadProof::mint(
            scope.clone(),
            requested.clone(),
            class,
            observed,
            self.nonce.clone(),
            entropy().unwrap_or([0xa5; 32]).to_vec(),
            self.clock.now(),
        )
    }
}

impl<C: Clock> ObjectStore for InMemoryObjectStore<C> {
    async fn put(
        &self,
        scope: &ExecutionScope,
        bytes: &[u8],
        media_type: &str,
    ) -> Result<VerifiedObjectRef, ObjectStoreError> {
        self.publish_if_absent(scope, bytes, media_type).await
    }

    async fn put_repairing(
        &self,
        scope: &ExecutionScope,
        bytes: &[u8],
        media_type: &str,
    ) -> Result<VerifiedObjectRef, ObjectStoreError> {
        if media_type.is_empty() || media_type.len() > 255 {
            return Err(ObjectStoreError::InvalidField);
        }
        let canonical;
        let bytes = if media_type == "application/json" {
            canonical = canonical_json(bytes)?;
            canonical.as_slice()
        } else {
            bytes
        };
        let object_digest = Digest::of(bytes);
        {
            let mut objects = self.objects.lock().expect("object lock poisoned");
            if let Some(existing) = objects.get_mut(&(scope.clone(), object_digest.clone())) {
                if existing.media_type != media_type {
                    return Err(ArtifactMetadataConflict {
                        digest: object_digest,
                        existing_size_bytes: existing.bytes.len() as u64,
                        candidate_size_bytes: bytes.len() as u64,
                    }
                    .into());
                }
                // A fresh completed verification proves wrong bytes. Replacing
                // them with independently hashed same-digest bytes restores the
                // immutable logical object; ordinary publish/get remain strict.
                if Digest::of(&existing.bytes) != object_digest {
                    existing.bytes = bytes.to_vec();
                }
            }
        }
        self.publish_if_absent(scope, bytes, media_type).await
    }

    async fn get(
        &self,
        scope: &ExecutionScope,
        requested: &Digest,
    ) -> Result<VerifiedObject, ObjectReadError> {
        let object = self
            .objects
            .lock()
            .expect("object lock poisoned")
            .get(&(scope.clone(), requested.clone()))
            .cloned()
            // An absent map entry is authoritative absence: the volatile store is
            // the whole world, so nothing can have prevented the lookup.
            .ok_or_else(|| {
                ObjectReadError::Corrupt(self.proof(
                    scope,
                    requested,
                    FailedReadClass::Missing,
                    None,
                ))
            })?;
        let observed = Digest::of(&object.bytes);
        if &observed != requested {
            // Verification completed and failed. Parity with FsObjectStore.
            return Err(ObjectReadError::Corrupt(self.proof(
                scope,
                requested,
                FailedReadClass::DigestInvalid,
                Some(observed),
            )));
        }
        Ok(VerifiedObject {
            reference: VerifiedObjectRef::new(
                scope.clone(),
                requested.clone(),
                object.bytes.len() as u64,
                object.media_type,
                object.object_key,
                self.nonce.clone(),
                object.bytes.clone(),
            ),
            bytes: object.bytes,
        })
    }

    async fn publish_if_absent(
        &self,
        scope: &ExecutionScope,
        bytes: &[u8],
        media_type: &str,
    ) -> Result<VerifiedObjectRef, ObjectStoreError> {
        if media_type.is_empty() || media_type.len() > 255 {
            return Err(ObjectStoreError::InvalidField);
        }
        let canonical;
        let bytes = if media_type == "application/json" {
            canonical = canonical_json(bytes)?;
            canonical.as_slice()
        } else {
            bytes
        };
        let object_digest = Digest::of(bytes);
        let key = (scope.clone(), object_digest.clone());
        let mut objects = self.objects.lock().expect("object lock poisoned");
        if let Some(existing) = objects.get(&key) {
            if existing.bytes != bytes || existing.media_type != media_type {
                return Err(ArtifactMetadataConflict {
                    digest: object_digest,
                    existing_size_bytes: existing.bytes.len() as u64,
                    candidate_size_bytes: bytes.len() as u64,
                }
                .into());
            }
            return Ok(VerifiedObjectRef::new(
                scope.clone(),
                key.1,
                bytes.len() as u64,
                media_type.to_owned(),
                existing.object_key.clone(),
                self.nonce.clone(),
                bytes.to_vec(),
            ));
        }
        let object_key = format!(
            "{}/{}/{}",
            scope.tenant_id.as_str(),
            scope.namespace.as_str(),
            object_digest.as_str()
        );
        objects.insert(
            key,
            MemoryObject {
                bytes: bytes.to_vec(),
                media_type: media_type.to_owned(),
                object_key: object_key.clone(),
            },
        );
        Ok(VerifiedObjectRef::new(
            scope.clone(),
            object_digest,
            bytes.len() as u64,
            media_type.to_owned(),
            object_key,
            self.nonce.clone(),
            bytes.to_vec(),
        ))
    }
}

struct StrictJson(Value);

impl<'de> Deserialize<'de> for StrictJson {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct StrictJsonVisitor;

        impl<'de> Visitor<'de> for StrictJsonVisitor {
            type Value = Value;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("an I-JSON value without duplicate object keys")
            }

            fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
                Ok(Value::Null)
            }

            fn visit_bool<E: de::Error>(self, value: bool) -> Result<Value, E> {
                Ok(Value::Bool(value))
            }

            fn visit_i64<E: de::Error>(self, value: i64) -> Result<Value, E> {
                if (value as f64) as i64 != value {
                    return Err(E::custom("integer is not exactly representable as f64"));
                }
                Ok(Value::Number(value.into()))
            }

            fn visit_u64<E: de::Error>(self, value: u64) -> Result<Value, E> {
                if (value as f64) as u64 != value {
                    return Err(E::custom("integer is not exactly representable as f64"));
                }
                Ok(Value::Number(value.into()))
            }

            fn visit_f64<E: de::Error>(self, value: f64) -> Result<Value, E> {
                serde_json::Number::from_f64(value)
                    .map(Value::Number)
                    .ok_or_else(|| E::custom("non-finite JSON number"))
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Value, E> {
                Ok(Value::String(value.to_owned()))
            }

            fn visit_string<E: de::Error>(self, value: String) -> Result<Value, E> {
                Ok(Value::String(value))
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut values: A) -> Result<Value, A::Error> {
                let mut result = Vec::new();
                while let Some(value) = values.next_element::<StrictJson>()? {
                    result.push(value.0);
                }
                Ok(Value::Array(result))
            }

            fn visit_map<A: MapAccess<'de>>(self, mut values: A) -> Result<Value, A::Error> {
                let mut result = serde_json::Map::new();
                while let Some(key) = values.next_key::<String>()? {
                    let value = values.next_value::<StrictJson>()?.0;
                    if result.insert(key, value).is_some() {
                        return Err(de::Error::custom("duplicate object key"));
                    }
                }
                Ok(Value::Object(result))
            }
        }

        deserializer.deserialize_any(StrictJsonVisitor).map(Self)
    }
}

fn canonical_json(bytes: &[u8]) -> Result<Vec<u8>, ObjectStoreError> {
    let value: StrictJson =
        serde_json::from_slice(bytes).map_err(|_| ObjectStoreError::InvalidField)?;
    serde_jcs::to_vec(&value.0).map_err(|_| ObjectStoreError::InvalidField)
}

/// The in-memory control-plane store. It is the same reducer the SQLite store
/// runs inside each transaction, holding its state in process memory.
pub type InMemoryStore<C> = crate::reducer::ReducerStore<C>;
