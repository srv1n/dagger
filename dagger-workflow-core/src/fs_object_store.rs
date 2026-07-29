//! Durable filesystem implementation of the scoped object-store contract.

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
use sha2::{Digest as _, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

const NONCE_BYTES: usize = 32;
const COPY_BUFFER_BYTES: usize = 64 * 1024;

/// Scope-confined, content-addressed storage rooted at a local filesystem directory.
///
/// The root owns `objects`, `tmp`, and `meta` directories. `meta/store-instance-nonce`
/// survives reopening the same root so verified references and failed-read proofs remain
/// bound to the same durable object-store identity.
pub struct FsObjectStore<C> {
    root: PathBuf,
    clock: Arc<C>,
    store_instance_nonce: Vec<u8>,
    proof_seed: [u8; NONCE_BYTES],
    next_proof_nonce: AtomicU64,
}

impl<C: Clock> FsObjectStore<C> {
    /// Creates or opens an object-store root using the supplied diagnostic clock.
    pub fn new(root: impl AsRef<Path>, clock: Arc<C>) -> Result<Self, ObjectStoreError> {
        Self::open(root, clock)
    }

    /// Opens or initializes an object-store root using the supplied diagnostic clock.
    pub fn open(root: impl AsRef<Path>, clock: Arc<C>) -> Result<Self, ObjectStoreError> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root).map_err(storage_error)?;
        let objects = root.join("objects");
        let temporary = root.join("tmp");
        let metadata = root.join("meta");
        fs::create_dir_all(&objects).map_err(storage_error)?;
        fs::create_dir_all(&temporary).map_err(storage_error)?;
        fs::create_dir_all(&metadata).map_err(storage_error)?;
        sync_directory(&root).map_err(storage_error)?;
        sync_directory(&objects).map_err(storage_error)?;
        sync_directory(&temporary).map_err(storage_error)?;
        sync_directory(&metadata).map_err(storage_error)?;

        Ok(Self {
            store_instance_nonce: load_or_create_nonce(&metadata, &temporary)?,
            proof_seed: random_bytes()?,
            next_proof_nonce: AtomicU64::new(0),
            root,
            clock,
        })
    }

    /// Returns the durable object-store root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn objects_root(&self) -> PathBuf {
        self.root.join("objects")
    }

    fn temporary_root(&self) -> PathBuf {
        self.root.join("tmp")
    }

    fn object_path(&self, scope: &ExecutionScope, digest: &Digest) -> PathBuf {
        let digest_hex = digest
            .as_str()
            .strip_prefix("sha256:")
            .expect("Digest always has the sha256 prefix");
        self.objects_root()
            .join(encode_scope_atom(scope.tenant_id.as_str()))
            .join(encode_scope_atom(scope.namespace.as_str()))
            .join(&digest_hex[..2])
            .join(digest_hex)
    }

    fn object_key(&self, scope: &ExecutionScope, digest: &Digest) -> String {
        let digest_hex = digest
            .as_str()
            .strip_prefix("sha256:")
            .expect("Digest always has the sha256 prefix");
        format!(
            "objects/{}/{}/{}/{}",
            encode_scope_atom(scope.tenant_id.as_str()),
            encode_scope_atom(scope.namespace.as_str()),
            &digest_hex[..2],
            digest_hex
        )
    }

    fn media_path(&self, scope: &ExecutionScope, digest: &Digest) -> PathBuf {
        let digest_hex = digest
            .as_str()
            .strip_prefix("sha256:")
            .expect("Digest always has the sha256 prefix");
        self.root
            .join("meta")
            .join("object-media")
            .join(encode_scope_atom(scope.tenant_id.as_str()))
            .join(encode_scope_atom(scope.namespace.as_str()))
            .join(&digest_hex[..2])
            .join(digest_hex)
    }

    fn temporary_path(&self) -> Result<PathBuf, ObjectStoreError> {
        let nonce = random_bytes()?;
        Ok(self.temporary_root().join(format!("put-{}", hex(&nonce))))
    }

    fn media_temporary_path(&self) -> Result<PathBuf, ObjectStoreError> {
        let nonce = random_bytes()?;
        Ok(self.temporary_root().join(format!("media-{}", hex(&nonce))))
    }

    fn proof(
        &self,
        scope: &ExecutionScope,
        requested: &Digest,
        class: FailedReadClass,
        observed: Option<Digest>,
    ) -> FailedReadProof {
        let sequence = self.next_proof_nonce.fetch_add(1, Ordering::Relaxed);
        let mut material = Vec::with_capacity(NONCE_BYTES + std::mem::size_of::<u64>());
        material.extend(self.proof_seed);
        material.extend(sequence.to_be_bytes());
        let proof_nonce = Sha256::digest(material).to_vec();
        FailedReadProof::mint(
            scope.clone(),
            requested.clone(),
            class,
            observed,
            self.store_instance_nonce.clone(),
            proof_nonce,
            self.clock.now(),
        )
    }

    fn verified_reference(
        &self,
        scope: &ExecutionScope,
        digest: Digest,
        bytes: Vec<u8>,
        media_type: &str,
    ) -> VerifiedObjectRef {
        VerifiedObjectRef::new(
            scope.clone(),
            digest.clone(),
            bytes.len() as u64,
            media_type.to_owned(),
            self.object_key(scope, &digest),
            self.store_instance_nonce.clone(),
            bytes,
        )
    }

    fn read_verified(&self, path: &Path) -> io::Result<ReadVerified> {
        let mut file = File::open(path)?;
        let mut hasher = Sha256::new();
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; COPY_BUFFER_BYTES];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
            bytes.extend_from_slice(&buffer[..count]);
        }
        Ok(ReadVerified {
            digest: digest_from_hash(hasher.finalize()),
            bytes,
        })
    }

    fn publish(
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
        let candidate_digest = digest(bytes);
        let final_path = self.object_path(scope, &candidate_digest);
        match fs::symlink_metadata(&final_path) {
            Ok(_) => {
                return self.verify_existing_candidate(scope, &candidate_digest, bytes, media_type);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Err(ObjectStoreError::StorageUnavailable),
        }

        let temporary = self.temporary_path()?;
        let media_temporary = self.media_temporary_path()?;
        write_and_sync(&temporary, bytes).map_err(storage_error)?;
        write_and_sync(&media_temporary, media_type.as_bytes()).map_err(storage_error)?;
        let parent = final_path
            .parent()
            .expect("object paths always have a parent");
        fs::create_dir_all(parent).map_err(storage_error)?;
        sync_directory(parent).map_err(storage_error)?;

        let media_path = self.media_path(scope, &candidate_digest);
        let media_parent = media_path
            .parent()
            .expect("media paths always have a parent");
        fs::create_dir_all(media_parent).map_err(storage_error)?;
        sync_directory(media_parent).map_err(storage_error)?;

        match fs::hard_link(&media_temporary, &media_path) {
            Ok(()) => {
                sync_directory(media_parent).map_err(storage_error)?;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(ObjectStoreError::StorageUnavailable),
        }
        let existing_media = read_media_type(&media_path).map_err(storage_error)?;
        if existing_media != media_type {
            remove_temp(&temporary);
            remove_temp(&media_temporary);
            return Err(ArtifactMetadataConflict {
                digest: candidate_digest,
                existing_size_bytes: bytes.len() as u64,
                candidate_size_bytes: bytes.len() as u64,
            }
            .into());
        }

        let created = match fs::hard_link(&temporary, &final_path) {
            Ok(()) => {
                sync_directory(parent).map_err(storage_error)?;
                true
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => false,
            Err(_) => return Err(ObjectStoreError::StorageUnavailable),
        };
        let verified = self.read_verified(&final_path).map_err(storage_error)?;
        if verified.digest != candidate_digest
            || verified.bytes.len() != bytes.len()
            || verified.bytes != bytes
        {
            let existing_size_bytes = verified.bytes.len() as u64;
            remove_temp(&temporary);
            remove_temp(&media_temporary);
            return if created {
                Err(ObjectStoreError::StorageUnavailable)
            } else {
                Err(ArtifactMetadataConflict {
                    digest: candidate_digest,
                    existing_size_bytes,
                    candidate_size_bytes: bytes.len() as u64,
                }
                .into())
            };
        }
        remove_temp(&temporary);
        remove_temp(&media_temporary);
        Ok(self.verified_reference(scope, candidate_digest, verified.bytes, media_type))
    }

    fn verify_existing_candidate(
        &self,
        scope: &ExecutionScope,
        candidate_digest: &Digest,
        bytes: &[u8],
        media_type: &str,
    ) -> Result<VerifiedObjectRef, ObjectStoreError> {
        let final_path = self.object_path(scope, candidate_digest);
        let verified = self.read_verified(&final_path).map_err(storage_error)?;
        let existing_media =
            read_media_type(&self.media_path(scope, candidate_digest)).map_err(storage_error)?;
        if verified.digest != *candidate_digest
            || verified.bytes.len() != bytes.len()
            || verified.bytes != bytes
            || existing_media != media_type
        {
            return Err(ArtifactMetadataConflict {
                digest: candidate_digest.clone(),
                existing_size_bytes: verified.bytes.len() as u64,
                candidate_size_bytes: bytes.len() as u64,
            }
            .into());
        }
        Ok(self.verified_reference(scope, candidate_digest.clone(), verified.bytes, media_type))
    }
}

impl<C: Clock> ObjectStore for FsObjectStore<C> {
    async fn put(
        &self,
        scope: &ExecutionScope,
        bytes: &[u8],
        media_type: &str,
    ) -> Result<VerifiedObjectRef, ObjectStoreError> {
        self.publish(scope, bytes, media_type)
    }

    async fn get(
        &self,
        scope: &ExecutionScope,
        requested: &Digest,
    ) -> Result<VerifiedObject, ObjectReadError> {
        let path = self.object_path(scope, requested);
        let verified = self.read_verified(&path).map_err(|_| ObjectReadError {
            proof: self.proof(scope, requested, FailedReadClass::Missing, None),
        })?;
        if &verified.digest != requested {
            return Err(ObjectReadError {
                proof: self.proof(
                    scope,
                    requested,
                    FailedReadClass::DigestInvalid,
                    Some(verified.digest),
                ),
            });
        }
        let media_path = self.media_path(scope, requested);
        let media_type = read_media_type(&media_path).map_err(|_| ObjectReadError {
            proof: self.proof(scope, requested, FailedReadClass::Missing, None),
        })?;
        let reference = self.verified_reference(
            scope,
            requested.clone(),
            verified.bytes.clone(),
            &media_type,
        );
        Ok(VerifiedObject {
            reference,
            bytes: verified.bytes,
        })
    }

    async fn publish_if_absent(
        &self,
        scope: &ExecutionScope,
        bytes: &[u8],
        media_type: &str,
    ) -> Result<VerifiedObjectRef, ObjectStoreError> {
        self.publish(scope, bytes, media_type)
    }
}

struct ReadVerified {
    digest: Digest,
    bytes: Vec<u8>,
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

fn write_and_sync(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn read_media_type(path: &Path) -> io::Result<String> {
    let media_type = String::from_utf8(fs::read(path)?)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "media type is not UTF-8"))?;
    if media_type.is_empty() || media_type.len() > 255 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "media type is invalid",
        ));
    }
    Ok(media_type)
}

fn remove_temp(path: &Path) {
    if fs::remove_file(path).is_ok() {
        if let Some(parent) = path.parent() {
            let _ = sync_directory(parent);
        }
    }
}

fn load_or_create_nonce(metadata: &Path, temporary: &Path) -> Result<Vec<u8>, ObjectStoreError> {
    let nonce_path = metadata.join("store-instance-nonce");
    match fs::read(&nonce_path) {
        Ok(nonce) => return valid_nonce(nonce),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(ObjectStoreError::StorageUnavailable),
    }

    let temporary_nonce = temporary.join(format!("nonce-{}", hex(&random_bytes()?)));
    let nonce = random_bytes()?;
    write_and_sync(&temporary_nonce, &nonce).map_err(storage_error)?;
    match fs::hard_link(&temporary_nonce, &nonce_path) {
        Ok(()) => {
            sync_directory(metadata).map_err(storage_error)?;
            remove_temp(&temporary_nonce);
            valid_nonce(nonce.to_vec())
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            remove_temp(&temporary_nonce);
            fs::read(&nonce_path)
                .map_err(storage_error)
                .and_then(valid_nonce)
        }
        Err(_) => Err(ObjectStoreError::StorageUnavailable),
    }
}

fn valid_nonce(nonce: Vec<u8>) -> Result<Vec<u8>, ObjectStoreError> {
    if nonce.len() == NONCE_BYTES {
        Ok(nonce)
    } else {
        Err(ObjectStoreError::StorageUnavailable)
    }
}

fn random_bytes() -> Result<[u8; NONCE_BYTES], ObjectStoreError> {
    let mut nonce = [0_u8; NONCE_BYTES];
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut nonce))
        .map_err(storage_error)?;
    Ok(nonce)
}

fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

fn digest(bytes: &[u8]) -> Digest {
    digest_from_hash(Sha256::digest(bytes))
}

fn digest_from_hash(hash: impl AsRef<[u8]>) -> Digest {
    Digest::new(format!("sha256:{}", hex(hash.as_ref()))).expect("SHA-256 is a valid digest")
}

fn encode_scope_atom(atom: &str) -> String {
    hex(atom.as_bytes())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn storage_error(_: io::Error) -> ObjectStoreError {
    ObjectStoreError::StorageUnavailable
}
