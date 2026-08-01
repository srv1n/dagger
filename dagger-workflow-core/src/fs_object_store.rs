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
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

const NONCE_BYTES: usize = 32;

/// The filesystem operations the durable publication protocol performs.
///
/// This exists as a seam because fsync is not observable through `std::fs`: no assertion made
/// over a real filesystem can fail when a durability barrier is deleted. Contract erratum 0.1.1
/// section C.2 states the binding property as a capability property -- no committable
/// `VerifiedObjectRef` may escape before its barriers complete -- and only a model filesystem
/// that tracks durability separately from namespace visibility can witness it. W11-D
/// (`tests/w11d_crash_model.rs`) implements that model over this trait.
///
/// The seam covers exactly what the store calls and nothing else; it is not a virtual
/// filesystem. Entropy (`/dev/urandom`) is deliberately outside it: nonces are identity, not
/// durability, and modelling them buys no protocol evidence.
pub trait StoreFs: Send + Sync {
    /// Creates a single directory, failing with `AlreadyExists` if the name is taken.
    fn create_dir(&self, path: &Path) -> io::Result<()>;

    /// Creates a directory and any missing ancestors, succeeding if it already exists.
    fn create_dir_all(&self, path: &Path) -> io::Result<()>;

    /// Creates and writes `path`, failing with `AlreadyExists` if it is taken.
    ///
    /// Establishes no durability of either the file's contents or its directory entry.
    fn write_new(&self, path: &Path, bytes: &[u8]) -> io::Result<()>;

    /// Links `link` to the inode behind `source`, never replacing an existing entry.
    fn hard_link(&self, source: &Path, link: &Path) -> io::Result<()>;

    /// Unlinks a file.
    fn remove_file(&self, path: &Path) -> io::Result<()>;

    /// Reads a whole file. `NotFound` is the only authoritative absence; see `ReadFailure`.
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;

    /// Whether `path` is a directory, without following a final symlink.
    fn symlink_is_dir(&self, path: &Path) -> io::Result<bool>;

    /// Whether `path` is a directory, following symlinks.
    fn metadata_is_dir(&self, path: &Path) -> io::Result<bool>;

    /// Makes a file's contents durable. Does NOT make its directory entry durable.
    fn sync_file(&self, path: &Path) -> io::Result<()>;

    /// Makes a directory's entries durable. Does NOT make the contents of those files durable.
    fn sync_dir(&self, path: &Path) -> io::Result<()>;
}

/// The real local filesystem, and the only backend inside the durability claim (erratum C.3).
#[derive(Debug, Default, Clone, Copy)]
pub struct StdFs;

impl StoreFs for StdFs {
    fn create_dir(&self, path: &Path) -> io::Result<()> {
        fs::create_dir(path)
    }

    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        fs::create_dir_all(path)
    }

    fn write_new(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?
            .write_all(bytes)
    }

    fn hard_link(&self, source: &Path, link: &Path) -> io::Result<()> {
        fs::hard_link(source, link)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        fs::remove_file(path)
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        fs::read(path)
    }

    fn symlink_is_dir(&self, path: &Path) -> io::Result<bool> {
        fs::symlink_metadata(path).map(|metadata| metadata.file_type().is_dir())
    }

    fn metadata_is_dir(&self, path: &Path) -> io::Result<bool> {
        fs::metadata(path).map(|metadata| metadata.is_dir())
    }

    fn sync_file(&self, path: &Path) -> io::Result<()> {
        File::open(path)?.sync_all()
    }

    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        File::open(path)?.sync_all()
    }
}

/// Scope-confined, content-addressed storage rooted at a local filesystem directory.
///
/// The root owns `objects`, `tmp`, and `meta` directories. `meta/store-instance-nonce`
/// survives reopening the same root so verified references and failed-read proofs remain
/// bound to the same durable object-store identity.
pub struct FsObjectStore<C, F = StdFs> {
    root: PathBuf,
    clock: Arc<C>,
    fs: F,
    store_instance_nonce: Vec<u8>,
    proof_seed: [u8; NONCE_BYTES],
    next_proof_nonce: AtomicU64,
}

impl<C: Clock> FsObjectStore<C, StdFs> {
    /// Creates or opens an object-store root using the supplied diagnostic clock.
    pub fn new(root: impl AsRef<Path>, clock: Arc<C>) -> Result<Self, ObjectStoreError> {
        Self::open(root, clock)
    }

    /// Opens or initializes an object-store root using the supplied diagnostic clock.
    pub fn open(root: impl AsRef<Path>, clock: Arc<C>) -> Result<Self, ObjectStoreError> {
        Self::open_with(root, clock, StdFs)
    }
}

impl<C: Clock, F: StoreFs> FsObjectStore<C, F> {
    /// Opens or initializes an object-store root over an explicit filesystem backend.
    pub fn open_with(
        root: impl AsRef<Path>,
        clock: Arc<C>,
        fs: F,
    ) -> Result<Self, ObjectStoreError> {
        let root = root.as_ref().to_path_buf();
        create_root_all_synced(&fs, &root).map_err(storage_error)?;
        let objects = root.join("objects");
        let temporary = root.join("tmp");
        let metadata = root.join("meta");
        fs.create_dir_all(&objects).map_err(storage_error)?;
        fs.create_dir_all(&temporary).map_err(storage_error)?;
        fs.create_dir_all(&metadata).map_err(storage_error)?;
        fs.sync_dir(&root).map_err(storage_error)?;
        fs.sync_dir(&objects).map_err(storage_error)?;
        fs.sync_dir(&temporary).map_err(storage_error)?;
        fs.sync_dir(&metadata).map_err(storage_error)?;

        Ok(Self {
            store_instance_nonce: load_or_create_nonce(&fs, &metadata, &temporary)?,
            proof_seed: random_bytes()?,
            next_proof_nonce: AtomicU64::new(0),
            root,
            clock,
            fs,
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

    /// Classifies a failed component read that would otherwise be authoritative absence.
    fn missing_or_unavailable(
        &self,
        scope: &ExecutionScope,
        requested: &Digest,
        failure: ReadFailure,
    ) -> ObjectReadError {
        match failure {
            ReadFailure::Absent => self.corrupt(scope, requested, FailedReadClass::Missing, None),
            ReadFailure::Unavailable => ObjectReadError::StorageUnavailable,
        }
    }

    /// Mints a corruption proof only while the durable store identity still matches the
    /// identity cached at open time.
    ///
    /// The cached nonce is read once at open. A remounted, replaced, or unmounted store can
    /// otherwise present a wholly different (or empty) directory tree as an authoritative
    /// absence under the old identity. Re-reading the durable nonce is the store's own check
    /// that it is still observing the instance it was opened against; failing to read it, or
    /// reading a different value, proves nothing about the object and so mints no proof.
    fn corrupt(
        &self,
        scope: &ExecutionScope,
        requested: &Digest,
        class: FailedReadClass,
        observed: Option<Digest>,
    ) -> ObjectReadError {
        match self
            .fs
            .read(&self.root.join("meta").join("store-instance-nonce"))
        {
            Ok(nonce) if nonce == self.store_instance_nonce => {
                ObjectReadError::Corrupt(self.proof(scope, requested, class, observed))
            }
            _ => ObjectReadError::StorageUnavailable,
        }
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

    /// Reads a whole object and digests it.
    ///
    /// Only a `NotFound` is authoritative absence. Every other failure is a failure of this
    /// store to complete a read: the returned digest is only ever the digest of a read that ran
    /// cleanly to EOF, so a mismatch can never be a partial read.
    fn read_verified(&self, path: &Path) -> Result<ReadVerified, ReadFailure> {
        let bytes = self.fs.read(path).map_err(ReadFailure::classify)?;
        Ok(ReadVerified {
            digest: digest(&bytes),
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
        match self.fs.symlink_is_dir(&final_path) {
            Ok(_) => {
                return self.verify_existing_candidate(scope, &candidate_digest, bytes, media_type);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Err(ObjectStoreError::StorageUnavailable),
        }

        let temporary = self.temporary_path()?;
        let media_temporary = self.media_temporary_path()?;
        write_and_sync(&self.fs, &temporary, bytes).map_err(storage_error)?;
        write_and_sync(&self.fs, &media_temporary, media_type.as_bytes()).map_err(storage_error)?;
        let parent = final_path
            .parent()
            .expect("object paths always have a parent");
        create_dir_all_synced(&self.fs, &self.objects_root(), parent).map_err(storage_error)?;

        let media_path = self.media_path(scope, &candidate_digest);
        let media_parent = media_path
            .parent()
            .expect("media paths always have a parent");
        create_dir_all_synced(&self.fs, &self.root.join("meta"), media_parent)
            .map_err(storage_error)?;

        match self.fs.hard_link(&media_temporary, &media_path) {
            Ok(()) => {
                self.fs.sync_dir(media_parent).map_err(storage_error)?;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                self.fs.sync_dir(media_parent).map_err(storage_error)?;
            }
            Err(_) => return Err(ObjectStoreError::StorageUnavailable),
        }
        let existing_media = read_media_type(&self.fs, &media_path).map_err(unavailable)?;
        if existing_media != media_type {
            remove_temp(&self.fs, &temporary);
            remove_temp(&self.fs, &media_temporary);
            return Err(ArtifactMetadataConflict {
                digest: candidate_digest,
                existing_size_bytes: bytes.len() as u64,
                candidate_size_bytes: bytes.len() as u64,
            }
            .into());
        }

        let created = match self.fs.hard_link(&temporary, &final_path) {
            Ok(()) => true,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => false,
            Err(_) => return Err(ObjectStoreError::StorageUnavailable),
        };
        let finalized =
            self.finalize_candidate(scope, &candidate_digest, bytes, media_type, created);
        remove_temp(&self.fs, &temporary);
        remove_temp(&self.fs, &media_temporary);
        finalized
    }

    fn verify_existing_candidate(
        &self,
        scope: &ExecutionScope,
        candidate_digest: &Digest,
        bytes: &[u8],
        media_type: &str,
    ) -> Result<VerifiedObjectRef, ObjectStoreError> {
        self.finalize_candidate(scope, candidate_digest, bytes, media_type, false)
    }

    /// Establishes the publication barrier for an object: the directory entries that name its
    /// data file and its media sidecar are durable once this returns.
    ///
    /// Every path that mints a `VerifiedObjectRef` crosses this, readers included. Raw namespace
    /// visibility before the publisher's own barrier is normal for a link-and-fsync protocol --
    /// the entry has to exist before its directory can be synced -- so a reader that finds an
    /// object may be seeing an entry the publisher has not yet made durable. A reader that
    /// returned a committable reference from that observation would let the control plane commit
    /// to an object a publisher crash could still erase, and a process-local lock cannot close it
    /// because the store is shared by many server processes. The reader therefore establishes the
    /// barrier itself.
    fn sync_publication_barrier(
        &self,
        scope: &ExecutionScope,
        digest: &Digest,
    ) -> Result<(), io::Error> {
        self.fs.sync_dir(
            self.object_path(scope, digest)
                .parent()
                .expect("object paths always have a parent"),
        )?;
        self.fs.sync_dir(
            self.media_path(scope, digest)
                .parent()
                .expect("media paths always have a parent"),
        )
    }

    /// Establishes the publication barrier before returning a verified ref.
    fn finalize_candidate(
        &self,
        scope: &ExecutionScope,
        candidate_digest: &Digest,
        bytes: &[u8],
        media_type: &str,
        created: bool,
    ) -> Result<VerifiedObjectRef, ObjectStoreError> {
        let final_path = self.object_path(scope, candidate_digest);
        self.sync_publication_barrier(scope, candidate_digest)
            .map_err(storage_error)?;
        let verified = self.read_verified(&final_path).map_err(unavailable)?;
        let existing_media = read_media_type(&self.fs, &self.media_path(scope, candidate_digest))
            .map_err(unavailable)?;
        if verified.digest != *candidate_digest
            || verified.bytes.len() != bytes.len()
            || verified.bytes != bytes
            || existing_media != media_type
        {
            return if created {
                Err(ObjectStoreError::StorageUnavailable)
            } else {
                Err(ArtifactMetadataConflict {
                    digest: candidate_digest.clone(),
                    existing_size_bytes: verified.bytes.len() as u64,
                    candidate_size_bytes: bytes.len() as u64,
                }
                .into())
            };
        }
        Ok(self.verified_reference(scope, candidate_digest.clone(), verified.bytes, media_type))
    }
}

impl<C: Clock, F: StoreFs> ObjectStore for FsObjectStore<C, F> {
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
        let verified = self
            .read_verified(&path)
            .map_err(|failure| self.missing_or_unavailable(scope, requested, failure))?;
        if &verified.digest != requested {
            // A complete stream that ran to EOF; only such a read can accuse the content.
            return Err(self.corrupt(
                scope,
                requested,
                FailedReadClass::DigestInvalid,
                Some(verified.digest),
            ));
        }
        let media_path = self.media_path(scope, requested);
        let media_type = read_media_type(&self.fs, &media_path)
            .map_err(|failure| self.missing_or_unavailable(scope, requested, failure))?;
        // The read path pays an fsync per successful get so that no committable reference can
        // escape ahead of the publication barrier. Splitting this return into a non-committable
        // verified-read capability, distinct from the durable put capability, would remove the
        // cost; that is a type-level change to the artifact contract, not to this store.
        self.sync_publication_barrier(scope, requested)
            .map_err(|_| ObjectReadError::StorageUnavailable)?;
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

/// Why a component read failed, in terms of what it is entitled to assert about the object.
///
/// A remote implementation of this store classifies the same way: a 404 locating the object is
/// `Absent`; 503, throttling, a reset connection, DNS failure, expired credentials, and any
/// error raised part-way through a body are `Unavailable`, because they are self-correcting
/// operational failures that must never invalidate a succeeded run.
enum ReadFailure {
    /// The component was authoritatively not there when it was looked up.
    Absent,
    /// The store could not complete the read. Asserts nothing about the object.
    Unavailable,
}

impl ReadFailure {
    /// Classifies a failed whole-file read.
    ///
    /// `NotFound` can only be raised while locating the component, so it is the only kind that
    /// observes an absence. PermissionDenied, descriptor exhaustion, EIO, IsADirectory, a stale
    /// mount, and every failure raised part-way through an already-open file observe nothing.
    fn classify(error: io::Error) -> Self {
        if error.kind() == io::ErrorKind::NotFound {
            Self::Absent
        } else {
            Self::Unavailable
        }
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

/// Writes a new file and makes its contents durable. Its directory entry is not durable yet.
fn write_and_sync(fs: &impl StoreFs, path: &Path, bytes: &[u8]) -> io::Result<()> {
    fs.write_new(path, bytes)?;
    fs.sync_file(path)
}

/// Creates missing descendants of an already-durable base, syncing each parent entry.
fn create_dir_all_synced(fs: &impl StoreFs, base: &Path, path: &Path) -> io::Result<()> {
    let relative = path
        .strip_prefix(base)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path escapes durable base"))?;
    let mut current = base.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "path escapes durable base",
            ));
        };
        create_directory_component(fs, &current, component)?;
        current.push(component);
    }
    Ok(())
}

/// Initializes `root`, syncing the parent of every component created or observed.
///
/// The upward walk resolves symlinks because the caller-supplied root, and every ancestor it
/// already has, is the caller's own trusted path (`/tmp` is a symlink on macOS). Components this
/// function creates are still rejected if they turn out to be symlinks, so nothing below the
/// durable base can escape it.
fn create_root_all_synced(fs: &impl StoreFs, root: &Path) -> io::Result<()> {
    let root = if root.is_absolute() {
        root.to_path_buf()
    } else {
        std::env::current_dir()?.join(root)
    };
    let mut missing = Vec::new();
    let mut current = root.as_path();
    loop {
        match fs.metadata_is_dir(current) {
            Ok(is_directory) => {
                if !is_directory {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "existing path is not a directory",
                    ));
                }
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let component = current.file_name().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "root has no existing ancestor")
                })?;
                missing.push(component.to_os_string());
                current = current.parent().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "root has no existing ancestor")
                })?;
            }
            Err(error) => return Err(error),
        }
    }

    let mut parent = current.to_path_buf();
    if missing.is_empty() {
        if let Some(root_parent) = parent.parent() {
            fs.sync_dir(root_parent)?;
        }
        return Ok(());
    }
    for component in missing.iter().rev() {
        create_directory_component(fs, &parent, component)?;
        parent.push(component);
    }
    Ok(())
}

/// Makes a directory component durable in `parent`, rejecting symlinks rather than following them.
fn create_directory_component(
    fs: &impl StoreFs,
    parent: &Path,
    component: &std::ffi::OsStr,
) -> io::Result<()> {
    let path = parent.join(component);
    match fs.create_dir(&path) {
        Ok(()) => fs.sync_dir(parent),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            if !fs.symlink_is_dir(&path)? {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "existing path is not a directory",
                ));
            }
            fs.sync_dir(parent)
        }
        Err(error) => Err(error),
    }
}

/// Reads the media sidecar, distinguishing its lookup phase from its read and decode phase.
///
/// Only an absent sidecar is authoritative absence. A sidecar that is present but empty,
/// oversized, or not UTF-8 is malformed metadata: the v0.1 proof vocabulary is closed and has
/// no class for it, so it is reported as an incomplete read rather than squeezed into
/// `Missing`.
fn read_media_type(fs: &impl StoreFs, path: &Path) -> Result<String, ReadFailure> {
    let bytes = fs.read(path).map_err(ReadFailure::classify)?;
    let Ok(media_type) = String::from_utf8(bytes) else {
        return Err(ReadFailure::Unavailable);
    };
    if media_type.is_empty() || media_type.len() > 255 {
        return Err(ReadFailure::Unavailable);
    }
    Ok(media_type)
}

fn remove_temp(fs: &impl StoreFs, path: &Path) {
    if fs.remove_file(path).is_ok() {
        if let Some(parent) = path.parent() {
            let _ = fs.sync_dir(parent);
        }
    }
}

fn load_or_create_nonce(
    fs: &impl StoreFs,
    metadata: &Path,
    temporary: &Path,
) -> Result<Vec<u8>, ObjectStoreError> {
    let nonce_path = metadata.join("store-instance-nonce");
    match fs.read(&nonce_path) {
        Ok(nonce) => {
            fs.sync_dir(metadata).map_err(storage_error)?;
            return valid_nonce(nonce);
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(ObjectStoreError::StorageUnavailable),
    }

    let temporary_nonce = temporary.join(format!("nonce-{}", hex(&random_bytes()?)));
    let nonce = random_bytes()?;
    write_and_sync(fs, &temporary_nonce, &nonce).map_err(storage_error)?;
    match fs.hard_link(&temporary_nonce, &nonce_path) {
        Ok(()) => {
            fs.sync_dir(metadata).map_err(storage_error)?;
            remove_temp(fs, &temporary_nonce);
            valid_nonce(nonce.to_vec())
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            remove_temp(fs, &temporary_nonce);
            let nonce = fs.read(&nonce_path).map_err(storage_error)?;
            fs.sync_dir(metadata).map_err(storage_error)?;
            valid_nonce(nonce)
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

/// Draws entropy from the host, outside the `StoreFs` seam: nonces are store identity, not
/// durability, so modelling them would prove nothing about the publication protocol.
fn random_bytes() -> Result<[u8; NONCE_BYTES], ObjectStoreError> {
    let mut nonce = [0_u8; NONCE_BYTES];
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut nonce))
        .map_err(storage_error)?;
    Ok(nonce)
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

/// Publication never mints proofs, so every phase of a failed read is equally unavailable.
fn unavailable(_: ReadFailure) -> ObjectStoreError {
    ObjectStoreError::StorageUnavailable
}

#[cfg(test)]
mod tests {
    use super::{create_dir_all_synced, create_root_all_synced, StdFs};
    use std::fs;
    use std::io;
    use std::path::PathBuf;
    use std::process;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

    fn test_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "dagger-fs-object-store-{label}-{}-{}",
            process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed),
        ))
    }

    #[test]
    fn synced_directory_creation_rejects_paths_outside_its_base() {
        let base = test_root("outside-base");
        fs::create_dir_all(&base).unwrap();
        let outside = base.parent().unwrap().join("outside");

        let error = create_dir_all_synced(&StdFs, &base, &outside).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        fs::remove_dir_all(base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn synced_directory_creation_rejects_symlink_components() {
        use std::os::unix::fs::symlink;

        let base = test_root("symlink-base");
        let outside = test_root("symlink-outside");
        fs::create_dir_all(&base).unwrap();
        fs::create_dir_all(&outside).unwrap();
        symlink(&outside, base.join("escape")).unwrap();

        let error =
            create_dir_all_synced(&StdFs, &base, &base.join("escape").join("child")).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        fs::remove_dir_all(base).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn root_creation_builds_every_missing_ancestor() {
        let base = test_root("missing-root");
        let root = base.join("nested").join("deep");

        create_root_all_synced(&StdFs, &root).unwrap();

        assert!(root.is_dir());
        // Proves the chain is created; the fsync of each new component's parent that this
        // helper exists for is not observable through std::fs.
        create_root_all_synced(&StdFs, &root).unwrap();
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn root_creation_rejects_an_existing_nondirectory() {
        let base = test_root("root-is-a-file");
        fs::write(&base, b"not a directory").unwrap();

        let error = create_root_all_synced(&StdFs, &base).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        fs::remove_file(base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn root_creation_accepts_a_symlinked_ancestor() {
        use std::os::unix::fs::symlink;

        // `/tmp` is a symlink on macOS, so a symlinked ancestor of the caller's own root
        // must not be an error; only components this code creates reject symlinks.
        let target = test_root("symlinked-ancestor-target");
        let link = test_root("symlinked-ancestor-link");
        fs::create_dir_all(&target).unwrap();
        symlink(&target, &link).unwrap();

        create_root_all_synced(&StdFs, &link.join("root")).unwrap();

        assert!(target.join("root").is_dir());
        fs::remove_file(link).unwrap();
        fs::remove_dir_all(target).unwrap();
    }
}
