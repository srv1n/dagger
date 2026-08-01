//! W11-D: stateful crash model over the local-filesystem publication protocol.
//!
//! # What this proves, and what it does not
//!
//! Every fsync in `src/fs_object_store.rs` was code-reviewed, never test-proven, because fsync is
//! not observable through `std::fs`: no assertion made against a real filesystem fails when a
//! barrier is deleted. This module replaces the real filesystem with a model that tracks
//! durability, so a deleted barrier becomes a failing test.
//!
//! The property under test is the one contract erratum 0.1.1 section C.2 states, not the
//! "no visible-before-durable window" that section retracts:
//!
//! > No successful publication response and no committable `VerifiedObjectRef` may escape before
//! > all required durability barriers and post-publication verification have completed.
//!
//! That is a capability property. The harness therefore records every `VerifiedObjectRef` that
//! escaped to a caller -- from `put` and from `get`, which mints one too -- crashes, restarts a
//! fresh store over the post-crash state, and requires each escaped reference to still resolve to
//! its exact bytes and media type.
//!
//! **Honest scope.** This is a userspace protocol model. It supports the claim "publication
//! protocol-model verified" and does NOT support "crash-durable on local filesystem X". Model
//! persistence rules are an assumption about POSIX filesystems, not a measurement of one; a real
//! kernel, mount, and device stack can violate them. A process SIGKILL would not close that gap
//! either -- the page cache and filesystem keep running after the process dies, so unsynced data
//! can still be written back. Only W11-E (abrupt-crash testing against a block device) qualifies
//! a named filesystem, and until it exists no filesystem is inside the durability claim
//! (erratum 0.1.1 section C.3).
//!
//! # Why the negative controls are the deliverable
//!
//! A model written by the same reasoning that wrote the barriers will confirm that reasoning.
//! `negative_control_*` below rebuilds the harness with each barrier mutated -- suppressed, or
//! moved after the successful return -- and requires the harness to FAIL. A green run of this
//! file means both that the protocol holds and that the harness can still see it break.

use dagger_workflow_core::artifact::{ObjectReadError, ObjectStore, ObjectStoreError};
use dagger_workflow_core::engine::TestClock;
use dagger_workflow_core::fs_object_store::{FsObjectStore, StoreFs};
use dagger_workflow_core::ids::{Digest, Timestamp};
use dagger_workflow_core::scope::{ExecutionScope, ScopeAtom};
use std::collections::BTreeMap;
use std::future::Future;
use std::io;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Component, Path, PathBuf};
use std::pin::pin;
use std::sync::{Arc, Mutex, MutexGuard, Once};
use std::task::{Context, Poll, Wake, Waker};

const ROOT: &str = "/store";
const OBJECTS: [(&[u8], &str); 2] = [
    (b"alpha object", "text/plain"),
    (b"beta object", "application/octet-stream"),
];

// ---------------------------------------------------------------------------
// Model filesystem
// ---------------------------------------------------------------------------

/// A directory entry. `durable` is durability of the ENTRY in its parent, which only an fsync of
/// the parent directory establishes -- never an fsync of the file itself.
#[derive(Clone, Debug)]
struct Entry {
    kind: Kind,
    durable: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Dir,
    File(u64),
}

/// An inode. `durable_data` is the contents as of the last `sync_file`, which is what a crash
/// leaves behind regardless of what was written since.
#[derive(Clone, Debug)]
struct Inode {
    data: Vec<u8>,
    durable_data: Option<Vec<u8>>,
}

/// Which unbarriered changes a particular crash happens to have persisted.
///
/// A model that discards every unsynced change is strictly weaker than a real filesystem, which
/// persists some unbarriered changes and loses others. Each mode is a permitted outcome, and the
/// harness runs every crash point under all of them.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Survival {
    /// Nothing unbarriered survives.
    LoseAll,
    /// Everything unbarriered survives, including data never fsynced (a late writeback).
    KeepAll,
    /// Directory entries survive but unsynced file contents do not: the classic
    /// delayed-allocation outcome where a file is present and empty.
    KeepEntriesLoseData,
    /// One subtree's unbarriered entries survive while the other's do not.
    KeepObjectsLoseMeta,
    KeepMetaLoseObjects,
    /// A deterministic per-path split, so entries in the same directory disagree.
    Split(u64),
}

const SURVIVALS: [Survival; 7] = [
    Survival::LoseAll,
    Survival::KeepAll,
    Survival::KeepEntriesLoseData,
    Survival::KeepObjectsLoseMeta,
    Survival::KeepMetaLoseObjects,
    Survival::Split(0x9e37),
    Survival::Split(0x5bf0),
];

impl Survival {
    fn entry_survives(self, path: &Path) -> bool {
        match self {
            Self::LoseAll => false,
            Self::KeepAll | Self::KeepEntriesLoseData => true,
            Self::KeepObjectsLoseMeta => under(path, "objects"),
            Self::KeepMetaLoseObjects => under(path, "meta"),
            Self::Split(seed) => (mix(path, seed) & 1) == 0,
        }
    }

    fn data_survives(self, path: &Path) -> bool {
        match self {
            Self::LoseAll | Self::KeepEntriesLoseData => false,
            Self::KeepAll => true,
            Self::KeepObjectsLoseMeta => under(path, "objects"),
            Self::KeepMetaLoseObjects => under(path, "meta"),
            Self::Split(seed) => (mix(path, seed) & 2) == 0,
        }
    }
}

fn under(path: &Path, subtree: &str) -> bool {
    path.strip_prefix(ROOT)
        .ok()
        .and_then(|relative| relative.components().next())
        .is_some_and(|first| first.as_os_str() == subtree)
}

/// FNV-1a over the path, so a split is deterministic and reproducible from the seed alone.
fn mix(path: &Path, seed: u64) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325 ^ seed;
    for byte in path.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash >> 7
}

/// The barrier a given fsync establishes, identified by the path it targets.
///
/// The store performs no other kind of fsync, so the path is a complete identification: every
/// `sync_file` is a temporary file, and every `sync_dir` is one of these three directory roles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Barrier {
    /// `sync_file(tmp/...)`: makes candidate contents durable before any entry names them.
    TempFile,
    /// `sync_dir(objects/<tenant>/<namespace>/<shard>)`: makes the data entry durable.
    ObjectShardDir,
    /// `sync_dir(meta/object-media/<tenant>/<namespace>/<shard>)`: makes the sidecar entry durable.
    MediaShardDir,
    /// Every other directory fsync: the root and its parent, `objects`, `tmp`, `meta`,
    /// `object-media`, the tenant and namespace levels, and the nonce.
    StructureDir,
}

const BARRIERS: [Barrier; 4] = [
    Barrier::TempFile,
    Barrier::ObjectShardDir,
    Barrier::MediaShardDir,
    Barrier::StructureDir,
];

fn directory_barrier(path: &Path) -> Barrier {
    let Ok(relative) = path.strip_prefix(ROOT) else {
        return Barrier::StructureDir;
    };
    let parts: Vec<_> = relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    match parts.as_slice() {
        [first, _tenant, _namespace, _shard] if first == "objects" => Barrier::ObjectShardDir,
        [first, second, _tenant, _namespace, _shard]
            if first == "meta" && second == "object-media" =>
        {
            Barrier::MediaShardDir
        }
        _ => Barrier::StructureDir,
    }
}

/// How a barrier behaves in this build. `Honest` is the shipped implementation.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Mutation {
    Honest,
    /// The fsync call is deleted from the source. Indistinguishable at the seam from a no-op
    /// implementation, so the two are the same effect here; `negative_control_removed` is
    /// separated from `negative_control_no_op` only by additionally requiring that the call site
    /// currently exists (the call is observed at least once), which is what "deleting it" needs
    /// in order to be a real mutation rather than a rename of nothing.
    Removed,
    /// The fsync is a no-op that still reports success.
    NoOp,
    /// The fsync is moved after the successful return: its effect is queued and applied only
    /// when the caller has already been handed the reference.
    Deferred,
}

/// A barrier whose effect has been queued to land after the successful return.
///
/// A file barrier names the inode, not the path: an fsync is issued against an open descriptor,
/// which stays valid after the temporary name is unlinked.
#[derive(Clone, Debug)]
enum DeferredBarrier {
    File(u64),
    Dir(PathBuf),
}

#[derive(Clone, Copy, Debug)]
struct Policy {
    target: Barrier,
    mutation: Mutation,
}

impl Policy {
    const HONEST: Self = Self {
        target: Barrier::TempFile,
        mutation: Mutation::Honest,
    };

    fn of(self, barrier: Barrier) -> Mutation {
        if barrier == self.target {
            self.mutation
        } else {
            Mutation::Honest
        }
    }
}

/// The crash sentinel. Unwinding is how a model crash abandons the store mid-operation; a
/// returned error would be a graceful failure, which is a different thing entirely.
struct CrashPanic;

fn silence_crash_panics() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let default = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            if info.payload().downcast_ref::<CrashPanic>().is_none() {
                default(info);
            }
        }));
    });
}

struct Model {
    entries: BTreeMap<PathBuf, Entry>,
    inodes: BTreeMap<u64, Inode>,
    /// Unlinks not yet made durable by an fsync of the parent. A crash may undo them.
    pending_removals: BTreeMap<PathBuf, Entry>,
    /// Barrier effects queued by `Mutation::Deferred`.
    deferred: Vec<DeferredBarrier>,
    next_inode: u64,
    /// Injectable points consumed so far. Every seam call consumes two: one before it applies
    /// (crash immediately before a mutation or a barrier) and one after (crash immediately after
    /// a successful mutation or barrier).
    step: u64,
    crash_at: Option<u64>,
    counting: bool,
    barrier_calls: BTreeMap<Barrier, u64>,
    policy: Policy,
}

impl Model {
    fn new(policy: Policy) -> Self {
        let mut entries = BTreeMap::new();
        // The root's own parent has to pre-exist: the store walks up to the first existing
        // ancestor and syncs it, exactly as it does under a real `/tmp`.
        entries.insert(
            PathBuf::from("/"),
            Entry {
                kind: Kind::Dir,
                durable: true,
            },
        );
        Self {
            entries,
            inodes: BTreeMap::new(),
            pending_removals: BTreeMap::new(),
            deferred: Vec::new(),
            next_inode: 0,
            step: 0,
            crash_at: None,
            counting: true,
            barrier_calls: BTreeMap::new(),
            policy,
        }
    }

    /// Consumes one injection point, crashing if this is the selected one.
    fn tick(&mut self) {
        if !self.counting {
            return;
        }
        let step = self.step;
        self.step += 1;
        if self.crash_at == Some(step) {
            panic::panic_any(CrashPanic);
        }
    }

    fn parent_is_dir(&self, path: &Path) -> io::Result<()> {
        let parent = path
            .parent()
            .ok_or_else(|| error(io::ErrorKind::InvalidInput, "no parent"))?;
        match self.entries.get(parent) {
            Some(entry) if entry.kind == Kind::Dir => Ok(()),
            Some(_) => Err(error(io::ErrorKind::NotFound, "parent is not a directory")),
            None => Err(error(io::ErrorKind::NotFound, "parent does not exist")),
        }
    }

    fn insert(&mut self, path: &Path, kind: Kind) {
        // A path that is written again after an unlink is live once more, so the crash can no
        // longer resurrect the old entry underneath it.
        self.pending_removals.remove(path);
        self.entries.insert(
            path.to_path_buf(),
            Entry {
                kind,
                durable: false,
            },
        );
    }

    fn file_inode(&self, path: &Path) -> io::Result<u64> {
        match self.entries.get(path) {
            Some(Entry {
                kind: Kind::File(inode),
                ..
            }) => Ok(*inode),
            Some(_) => Err(error(io::ErrorKind::Other, "is a directory")),
            None => Err(error(io::ErrorKind::NotFound, "no such file")),
        }
    }

    /// Applies a directory barrier: every entry currently in `path` becomes durable, and every
    /// unlink from `path` becomes final. File contents are untouched -- that is the whole point
    /// of modelling the two separately.
    fn apply_dir_sync(&mut self, path: &Path) {
        let children: Vec<PathBuf> = self
            .entries
            .keys()
            .filter(|candidate| candidate.parent() == Some(path))
            .cloned()
            .collect();
        for child in children {
            if let Some(entry) = self.entries.get_mut(&child) {
                entry.durable = true;
            }
        }
        self.pending_removals
            .retain(|removed, _| removed.parent() != Some(path));
    }

    fn barrier(
        &mut self,
        barrier: Barrier,
        queued: DeferredBarrier,
        apply: impl FnOnce(&mut Self),
    ) {
        *self.barrier_calls.entry(barrier).or_default() += 1;
        match self.policy.of(barrier) {
            Mutation::Honest => apply(self),
            Mutation::Removed | Mutation::NoOp => {}
            Mutation::Deferred => self.deferred.push(queued),
        }
    }

    /// Applies every barrier `Mutation::Deferred` queued. The harness calls this only once the
    /// workload has completed, which is what "moved after the successful return" means.
    fn flush_deferred(&mut self) {
        for barrier in std::mem::take(&mut self.deferred) {
            match barrier {
                DeferredBarrier::Dir(path) => self.apply_dir_sync(&path),
                DeferredBarrier::File(inode) => {
                    if let Some(node) = self.inodes.get_mut(&inode) {
                        node.durable_data = Some(node.data.clone());
                    }
                }
            }
        }
    }

    /// Rewrites the state as a crash would leave it, under one permitted persistence outcome.
    fn crash(&mut self, survival: Survival) {
        self.deferred.clear();

        // An unlink that never reached the disk can come back.
        for (path, entry) in std::mem::take(&mut self.pending_removals) {
            if !survival.entry_survives(&path) {
                self.entries.insert(path, entry);
            }
        }

        // Unbarriered entries survive or vanish per the outcome; durable ones always survive.
        self.entries
            .retain(|path, entry| entry.durable || survival.entry_survives(path));

        // Contents revert to the last fsync of the file. Never-synced contents survive only if
        // the outcome says the writeback happened to reach the disk; otherwise the entry is left
        // naming an empty file, which is what an allocated-but-unwritten inode looks like.
        let live: Vec<(PathBuf, u64)> = self
            .entries
            .iter()
            .filter_map(|(path, entry)| match entry.kind {
                Kind::File(inode) => Some((path.clone(), inode)),
                Kind::Dir => None,
            })
            .collect();
        for (path, inode) in live {
            let node = self
                .inodes
                .get_mut(&inode)
                .expect("live entry has an inode");
            if let Some(durable) = node.durable_data.clone() {
                node.data = durable;
            } else if !survival.data_survives(&path) {
                node.data = Vec::new();
            }
        }

        // A surviving entry under a vanished directory is not reachable.
        loop {
            let orphans: Vec<PathBuf> = self
                .entries
                .keys()
                .filter(|path| {
                    path.parent()
                        .is_some_and(|parent| !self.entries.contains_key(parent))
                })
                .cloned()
                .collect();
            if orphans.is_empty() {
                break;
            }
            for orphan in orphans {
                self.entries.remove(&orphan);
            }
        }

        // Whatever came through the crash is by definition on disk now.
        for entry in self.entries.values_mut() {
            entry.durable = true;
        }
        let reachable: Vec<u64> = self
            .entries
            .values()
            .filter_map(|entry| match entry.kind {
                Kind::File(inode) => Some(inode),
                Kind::Dir => None,
            })
            .collect();
        self.inodes.retain(|inode, _| reachable.contains(inode));
        for node in self.inodes.values_mut() {
            node.durable_data = Some(node.data.clone());
        }
    }
}

fn error(kind: io::ErrorKind, message: &'static str) -> io::Error {
    io::Error::new(kind, message)
}

/// A handle onto one model filesystem. Cloning shares the state, so a "restart" can build a fresh
/// store over exactly the state the crash left.
#[derive(Clone)]
struct ModelFs(Arc<Mutex<Model>>);

impl ModelFs {
    fn new(policy: Policy) -> Self {
        Self(Arc::new(Mutex::new(Model::new(policy))))
    }

    /// A crash unwinds through the lock, so poisoning is expected and meaningless here.
    fn lock(&self) -> MutexGuard<'_, Model> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Runs one seam operation between two injection points.
    fn operation<T>(&self, act: impl FnOnce(&mut Model) -> io::Result<T>) -> io::Result<T> {
        self.lock().tick();
        let result = act(&mut self.lock());
        if result.is_ok() {
            self.lock().tick();
        }
        result
    }
}

impl StoreFs for ModelFs {
    fn create_dir(&self, path: &Path) -> io::Result<()> {
        self.operation(|model| {
            if model.entries.contains_key(path) {
                return Err(error(io::ErrorKind::AlreadyExists, "exists"));
            }
            model.parent_is_dir(path)?;
            model.insert(path, Kind::Dir);
            Ok(())
        })
    }

    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        self.operation(|model| {
            let mut current = PathBuf::from("/");
            for component in path.components() {
                if let Component::Normal(part) = component {
                    current.push(part);
                    match model.entries.get(&current) {
                        Some(entry) if entry.kind == Kind::Dir => {}
                        Some(_) => return Err(error(io::ErrorKind::AlreadyExists, "not a dir")),
                        None => model.insert(&current, Kind::Dir),
                    }
                }
            }
            Ok(())
        })
    }

    fn write_new(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        self.operation(|model| {
            if model.entries.contains_key(path) {
                return Err(error(io::ErrorKind::AlreadyExists, "exists"));
            }
            model.parent_is_dir(path)?;
            let inode = model.next_inode;
            model.next_inode += 1;
            model.inodes.insert(
                inode,
                Inode {
                    data: bytes.to_vec(),
                    durable_data: None,
                },
            );
            model.insert(path, Kind::File(inode));
            Ok(())
        })
    }

    fn hard_link(&self, source: &Path, link: &Path) -> io::Result<()> {
        self.operation(|model| {
            let inode = model.file_inode(source)?;
            if model.entries.contains_key(link) {
                return Err(error(io::ErrorKind::AlreadyExists, "exists"));
            }
            model.parent_is_dir(link)?;
            model.insert(link, Kind::File(inode));
            Ok(())
        })
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.operation(|model| {
            model.file_inode(path)?;
            let entry = model.entries.remove(path).expect("checked above");
            if entry.durable {
                model.pending_removals.insert(path.to_path_buf(), entry);
            }
            Ok(())
        })
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.operation(|model| {
            let inode = model.file_inode(path)?;
            Ok(model.inodes[&inode].data.clone())
        })
    }

    fn symlink_is_dir(&self, path: &Path) -> io::Result<bool> {
        self.operation(|model| match model.entries.get(path) {
            Some(entry) => Ok(entry.kind == Kind::Dir),
            None => Err(error(io::ErrorKind::NotFound, "no such path")),
        })
    }

    fn metadata_is_dir(&self, path: &Path) -> io::Result<bool> {
        self.symlink_is_dir(path)
    }

    fn sync_file(&self, path: &Path) -> io::Result<()> {
        self.operation(|model| {
            let inode = model.file_inode(path)?;
            model.barrier(Barrier::TempFile, DeferredBarrier::File(inode), |model| {
                let node = model.inodes.get_mut(&inode).expect("checked above");
                node.durable_data = Some(node.data.clone());
            });
            Ok(())
        })
    }

    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        self.operation(|model| {
            match model.entries.get(path) {
                Some(entry) if entry.kind == Kind::Dir => {}
                Some(_) => return Err(error(io::ErrorKind::Other, "not a directory")),
                None => return Err(error(io::ErrorKind::NotFound, "no such directory")),
            }
            let owned = path.to_path_buf();
            let queued = DeferredBarrier::Dir(owned.clone());
            model.barrier(directory_barrier(path), queued, move |model| {
                model.apply_dir_sync(&owned)
            });
            Ok(())
        })
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

fn scope() -> ExecutionScope {
    ExecutionScope {
        tenant_id: ScopeAtom::new("tenant-a").unwrap(),
        namespace: ScopeAtom::new("workflow").unwrap(),
    }
}

fn block_on<F: Future>(future: F) -> F::Output {
    struct Noop;
    impl Wake for Noop {
        fn wake(self: Arc<Self>) {}
    }
    let waker = Waker::from(Arc::new(Noop));
    let mut context = Context::from_waker(&waker);
    let mut future = pin!(future);
    match future.as_mut().poll(&mut context) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("filesystem object-store futures must not suspend"),
    }
}

/// Every restart builds a wholly fresh store, so no in-memory nonce, path, or cache survives.
fn open(fs: &ModelFs) -> Result<FsObjectStore<TestClock, ModelFs>, ObjectStoreError> {
    FsObjectStore::open_with(ROOT, Arc::new(TestClock::new(Timestamp(1))), fs.clone())
}

/// A committable reference that reached a caller. Erratum 0.1.1 C.2 binds the store to it.
#[derive(Clone, Debug)]
struct Escaped {
    digest: Digest,
    bytes: Vec<u8>,
    media_type: String,
    origin: &'static str,
}

/// The interleaved reader: a second process that knows the digest and calls `get` while the
/// publisher is mid-protocol. Erratum 0.1.1 C.2 names this the case a process-local lock cannot
/// close; `get` must establish the publication barrier itself before minting a reference.
fn interleaved_reader(fs: &ModelFs, digests: &[Digest], escaped: &Mutex<Vec<Escaped>>) {
    let (crash_at, counting) = {
        let mut model = fs.lock();
        let saved = (model.crash_at.take(), model.counting);
        // The reader is a different process: its syscalls are not points in the publisher's
        // step space, and it must not crash at the publisher's crash point.
        model.counting = false;
        saved
    };
    if let Ok(store) = open(fs) {
        for digest in digests {
            if let Ok(object) = block_on(store.get(&scope(), digest)) {
                escaped.lock().unwrap().push(Escaped {
                    digest: digest.clone(),
                    bytes: object.bytes,
                    media_type: object.reference.media_type().to_owned(),
                    origin: "get (interleaved reader)",
                });
            }
        }
    }
    let mut model = fs.lock();
    model.crash_at = crash_at;
    model.counting = counting;
}

/// The publishing workload. Two objects, then a replay of the first through the existing-object
/// fast path, then a read of each.
fn workload(fs: &ModelFs, escaped: &Mutex<Vec<Escaped>>) {
    let Ok(store) = open(fs) else {
        return;
    };
    for (bytes, media_type) in OBJECTS {
        if let Ok(reference) = block_on(store.put(&scope(), bytes, media_type)) {
            escaped.lock().unwrap().push(Escaped {
                digest: reference.digest().clone(),
                bytes: bytes.to_vec(),
                media_type: media_type.to_owned(),
                origin: "put",
            });
        }
    }
    let (bytes, media_type) = OBJECTS[0];
    if let Ok(reference) = block_on(store.publish_if_absent(&scope(), bytes, media_type)) {
        escaped.lock().unwrap().push(Escaped {
            digest: reference.digest().clone(),
            bytes: bytes.to_vec(),
            media_type: media_type.to_owned(),
            origin: "publish_if_absent (replay)",
        });
    }
}

struct Baseline {
    steps: u64,
    digests: Vec<Digest>,
    barrier_calls: BTreeMap<Barrier, u64>,
}

/// Runs the workload to completion with no crash, to learn the step space and the digests an
/// interleaved reader would know.
fn baseline(policy: Policy) -> Baseline {
    let fs = ModelFs::new(policy);
    let escaped = Mutex::new(Vec::new());
    workload(&fs, &escaped);
    let digests = escaped
        .lock()
        .unwrap()
        .iter()
        .map(|reference| reference.digest.clone())
        .collect();
    let model = fs.lock();
    Baseline {
        steps: model.step,
        digests,
        barrier_calls: model.barrier_calls.clone(),
    }
}

/// One crash fixture: crash the publisher at `crash_at`, with an interleaved reader at that same
/// point, then verify every escaped reference survives.
///
/// Returns the violation as text, or `Ok(())`.
fn fixture(
    policy: Policy,
    crash_at: u64,
    survival: Survival,
    digests: &[Digest],
) -> Result<(), String> {
    silence_crash_panics();
    let fs = ModelFs::new(policy);
    let escaped = Mutex::new(Vec::new());

    // Phase A: publish until the crash point.
    fs.lock().crash_at = Some(crash_at);
    let completed = match panic::catch_unwind(AssertUnwindSafe(|| workload(&fs, &escaped))) {
        Ok(()) => true,
        Err(payload) if payload.downcast_ref::<CrashPanic>().is_some() => {
            // The publisher is gone, mid-protocol. A second process that already knows the
            // digest may still find the entry and mint a reference from it; whatever it can
            // commit to has to survive the crash that is about to happen.
            interleaved_reader(&fs, digests, &escaped);
            false
        }
        Err(payload) => panic::resume_unwind(payload),
    };
    if completed {
        // The workload ran to the end, so a barrier moved after the successful return has had
        // its chance to run before the crash.
        fs.lock().flush_deferred();
    }

    fs.lock().crash_at = None;
    fs.lock().crash(survival);

    // Phase B: a fresh process over the post-crash state.
    let survivors = escaped.lock().unwrap().clone();
    let store = open(&fs).map_err(|error| {
        format!(
            "restart failed after the crash: {error:?}, with {} escaped reference(s)",
            survivors.len()
        )
    });
    let store = match store {
        Ok(store) => store,
        Err(message) => {
            return if survivors.is_empty() {
                Ok(())
            } else {
                Err(message)
            }
        }
    };

    for reference in &survivors {
        match block_on(store.get(&scope(), &reference.digest)) {
            Ok(object) => {
                if object.bytes != reference.bytes {
                    return Err(format!(
                        "escaped reference from {} resolves to different bytes after the crash: {:?} != {:?}",
                        reference.origin, object.bytes, reference.bytes
                    ));
                }
                if object.reference.media_type() != reference.media_type {
                    return Err(format!(
                        "escaped reference from {} resolves to media type {:?}, published as {:?}",
                        reference.origin,
                        object.reference.media_type(),
                        reference.media_type
                    ));
                }
            }
            Err(failure) => {
                return Err(format!(
                    "escaped reference from {} for {} is unreadable after the crash: {}",
                    reference.origin,
                    reference.digest.as_str(),
                    describe(&failure)
                ))
            }
        }
    }

    // Retry convergence: a republication after the crash either succeeds and is then readable,
    // or fails. It may never report success over an object it cannot resolve.
    for (bytes, media_type) in OBJECTS {
        if let Ok(reference) = block_on(store.publish_if_absent(&scope(), bytes, media_type)) {
            match block_on(store.get(&scope(), reference.digest())) {
                Ok(object) if object.bytes == bytes => {}
                Ok(object) => {
                    return Err(format!(
                        "retry after the crash published {:?} but reads back {:?}",
                        bytes, object.bytes
                    ))
                }
                Err(failure) => {
                    return Err(format!(
                        "retry after the crash reported success but the object is unreadable: {}",
                        describe(&failure)
                    ))
                }
            }
        }
    }
    Ok(())
}

fn describe(failure: &ObjectReadError) -> String {
    match failure {
        ObjectReadError::Corrupt(proof) => format!("{:?}", proof.error_class()),
        ObjectReadError::StorageUnavailable => "StorageUnavailable".to_owned(),
    }
}

/// One violated fixture, kept with the crash point and outcome that produced it.
struct Violation {
    crash_at: u64,
    steps: u64,
    survival: Survival,
    detail: String,
}

impl std::fmt::Display for Violation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "crash at step {}/{} under {:?}: {}",
            self.crash_at, self.steps, self.survival, self.detail
        )
    }
}

/// Sweeps every crash point against every permitted persistence outcome.
///
/// The sweep does not stop at the first violation: a negative control that reported only one
/// would say nothing about whether the barrier matters on more than a single fixture.
fn sweep(policy: Policy) -> Vec<Violation> {
    let baseline = baseline(policy);
    let mut violations = Vec::new();
    for crash_at in 0..=baseline.steps {
        for survival in SURVIVALS {
            if let Err(detail) = fixture(policy, crash_at, survival, &baseline.digests) {
                violations.push(Violation {
                    crash_at,
                    steps: baseline.steps,
                    survival,
                    detail,
                });
            }
        }
    }
    violations
}

/// Renders the distinct failure modes in a sweep, with one witnessing fixture each.
fn summarize(violations: &[Violation]) -> String {
    let mut distinct: Vec<&Violation> = Vec::new();
    for violation in violations {
        if !distinct.iter().any(|kept| kept.detail == violation.detail) {
            distinct.push(violation);
        }
    }
    let lines: Vec<String> = distinct
        .iter()
        .take(4)
        .map(|violation| format!("    {violation}"))
        .collect();
    format!(
        "{} failing fixture(s), {} distinct failure mode(s):\n{}",
        violations.len(),
        distinct.len(),
        lines.join("\n")
    )
}

// ---------------------------------------------------------------------------
// Gate 1: the model itself is not vacuous
// ---------------------------------------------------------------------------

#[test]
fn the_model_distinguishes_data_durability_from_entry_durability() {
    let fs = ModelFs::new(Policy::HONEST);
    let directory = PathBuf::from(ROOT);
    fs.create_dir(&directory).unwrap();
    fs.sync_dir(Path::new("/")).unwrap();
    let file = directory.join("candidate");
    fs.write_new(&file, b"contents").unwrap();

    // An fsync of the file makes its contents durable but does not link it.
    fs.sync_file(&file).unwrap();
    let mut lost = ModelFs(Arc::new(Mutex::new(clone_model(&fs))));
    lost.lock().crash(Survival::LoseAll);
    assert!(lost.read(&file).is_err(), "an unbarriered entry can vanish");

    // An fsync of the directory links it, and the contents are already durable.
    fs.sync_dir(&directory).unwrap();
    lost = ModelFs(Arc::new(Mutex::new(clone_model(&fs))));
    lost.lock().crash(Survival::LoseAll);
    assert_eq!(lost.read(&file).unwrap(), b"contents");

    // The reverse order loses the contents: the entry is durable, the data was never synced.
    let other = directory.join("unsynced");
    fs.write_new(&other, b"contents").unwrap();
    fs.sync_dir(&directory).unwrap();
    lost = ModelFs(Arc::new(Mutex::new(clone_model(&fs))));
    lost.lock().crash(Survival::KeepEntriesLoseData);
    assert_eq!(
        lost.read(&other).unwrap(),
        b"",
        "a linked file whose data was never fsynced is present and empty"
    );
}

#[test]
fn the_model_does_not_discard_every_unsynced_change() {
    let fs = ModelFs::new(Policy::HONEST);
    fs.create_dir(Path::new(ROOT)).unwrap();
    fs.sync_dir(Path::new("/")).unwrap();
    for directory in ["objects", "meta"] {
        fs.create_dir(&Path::new(ROOT).join(directory)).unwrap();
    }
    fs.sync_dir(Path::new(ROOT)).unwrap();
    let object = Path::new(ROOT).join("objects").join("entry");
    let media = Path::new(ROOT).join("meta").join("entry");
    fs.write_new(&object, b"o").unwrap();
    fs.write_new(&media, b"m").unwrap();
    fs.sync_file(&object).unwrap();
    fs.sync_file(&media).unwrap();

    // Neither entry is barriered. A filesystem may keep both, drop both, or split them.
    let keep = ModelFs(Arc::new(Mutex::new(clone_model(&fs))));
    keep.lock().crash(Survival::KeepAll);
    assert!(keep.read(&object).is_ok() && keep.read(&media).is_ok());

    let split = ModelFs(Arc::new(Mutex::new(clone_model(&fs))));
    split.lock().crash(Survival::KeepObjectsLoseMeta);
    assert!(
        split.read(&object).is_ok() && split.read(&media).is_err(),
        "one directory must be able to keep an unbarriered entry while another loses one"
    );
}

fn clone_model(fs: &ModelFs) -> Model {
    let model = fs.lock();
    Model {
        entries: model.entries.clone(),
        inodes: model.inodes.clone(),
        pending_removals: model.pending_removals.clone(),
        deferred: model.deferred.clone(),
        next_inode: model.next_inode,
        step: model.step,
        crash_at: None,
        counting: model.counting,
        barrier_calls: model.barrier_calls.clone(),
        policy: model.policy,
    }
}

#[test]
fn the_workload_exercises_every_barrier() {
    let baseline = baseline(Policy::HONEST);
    for barrier in BARRIERS {
        assert!(
            baseline.barrier_calls.get(&barrier).copied().unwrap_or(0) > 0,
            "no fixture reaches {barrier:?}, so mutating it would prove nothing"
        );
    }
    assert!(
        baseline.steps > 100,
        "the step space collapsed to {} points",
        baseline.steps
    );
    assert_eq!(
        baseline.digests.len(),
        3,
        "the workload must publish and replay"
    );
}

// ---------------------------------------------------------------------------
// Gate 2: the protocol holds
// ---------------------------------------------------------------------------

#[test]
fn no_committable_reference_escapes_before_its_barriers_complete() {
    // Erratum 0.1.1 section C.2. Every crash point, every permitted persistence outcome, with an
    // interleaved second-process reader at the crash point.
    let violations = sweep(Policy::HONEST);
    assert!(violations.is_empty(), "{}", summarize(&violations));
}

// ---------------------------------------------------------------------------
// Gate 3: negative controls
// ---------------------------------------------------------------------------

/// Asserts that mutating one barrier breaks at least one crash fixture, and reports which.
fn expect_violation(target: Barrier, mutation: Mutation) -> String {
    let policy = Policy { target, mutation };
    if mutation == Mutation::Removed {
        let calls = baseline(policy)
            .barrier_calls
            .get(&target)
            .copied()
            .unwrap_or(0);
        assert!(calls > 0, "{target:?} has no call site to remove");
    }
    let violations = sweep(policy);
    assert!(
        !violations.is_empty(),
        "MUTATION SURVIVED: {target:?} under {mutation:?} broke no crash fixture. The harness \
         cannot see this barrier, so it proves nothing about it."
    );
    summarize(&violations)
}

#[test]
fn negative_control_removed() {
    for target in BARRIERS {
        let violation = expect_violation(target, Mutation::Removed);
        println!(
            "
=== {target:?} removed ===
{violation}"
        );
    }
}

#[test]
fn negative_control_no_op() {
    for target in BARRIERS {
        let violation = expect_violation(target, Mutation::NoOp);
        println!(
            "
=== {target:?} no-op ===
{violation}"
        );
    }
}

#[test]
fn negative_control_moved_after_the_successful_return() {
    for target in BARRIERS {
        let violation = expect_violation(target, Mutation::Deferred);
        println!(
            "
=== {target:?} deferred ===
{violation}"
        );
    }
}

#[test]
fn deferring_a_barrier_is_a_weaker_mutation_than_suppressing_it() {
    // The distinction is what makes "moved after the return" its own control: a run that reaches
    // the end and only then crashes is safe when the barrier was merely late, and unsafe when it
    // was suppressed. If these ever coincide, the deferred control has silently become a copy of
    // the suppressed one.
    let baseline = baseline(Policy::HONEST);
    let at_the_end = baseline.steps;
    for target in [
        Barrier::ObjectShardDir,
        Barrier::MediaShardDir,
        Barrier::TempFile,
    ] {
        let deferred = Policy {
            target,
            mutation: Mutation::Deferred,
        };
        let removed = Policy {
            target,
            mutation: Mutation::Removed,
        };
        assert!(
            fixture(deferred, at_the_end, Survival::LoseAll, &baseline.digests).is_ok(),
            "{target:?}: a late barrier that still ran before the crash must be safe"
        );
        assert!(
            fixture(removed, at_the_end, Survival::LoseAll, &baseline.digests).is_err(),
            "{target:?}: a suppressed barrier must not be safe at the same crash point"
        );
    }
}
