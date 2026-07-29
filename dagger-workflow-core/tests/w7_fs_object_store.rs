use dagger_workflow_core::artifact::{FailedReadClass, ObjectStore, ObjectStoreError};
use dagger_workflow_core::engine::TestClock;
use dagger_workflow_core::fs_object_store::FsObjectStore;
use dagger_workflow_core::ids::{Digest, Timestamp};
use dagger_workflow_core::scope::{ExecutionScope, ScopeAtom};
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};

static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

fn scope() -> ExecutionScope {
    ExecutionScope {
        tenant_id: ScopeAtom::new("tenant-a").unwrap(),
        namespace: ScopeAtom::new("workflow").unwrap(),
    }
}

fn open_store(root: &Path) -> FsObjectStore<TestClock> {
    FsObjectStore::open(root, Arc::new(TestClock::new(Timestamp(1)))).unwrap()
}

fn object_file(root: &Path) -> PathBuf {
    let mut files = Vec::new();
    collect_files(&root.join("objects"), &mut files);
    assert_eq!(files.len(), 1, "exactly one final object is expected");
    files.pop().unwrap()
}

fn media_file(root: &Path, object: &Path) -> PathBuf {
    root.join("meta")
        .join("object-media")
        .join(object.strip_prefix(root.join("objects")).unwrap())
}

fn collect_files(path: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(path).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_files(&path, files);
        } else {
            files.push(path);
        }
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

struct TestRoot(PathBuf);

impl TestRoot {
    fn new(label: &str) -> Self {
        let id = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("dagger-w7-{label}-{}-{id}", process::id()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[tokio::test]
async fn application_json_is_canonicalized_before_digest_and_storage() {
    let root = TestRoot::new("canonical-json");
    let store = open_store(root.path());
    let scope = scope();
    let unordered = store
        .put(&scope, br#"{ "b": 1, "a": [3, 2, 1] }"#, "application/json")
        .await
        .unwrap();
    let ordered = store
        .publish_if_absent(&scope, br#"{"a":[3,2,1],"b":1}"#, "application/json")
        .await
        .unwrap();

    assert_eq!(unordered.digest(), ordered.digest());
    assert_eq!(
        fs::read(object_file(root.path())).unwrap(),
        br#"{"a":[3,2,1],"b":1}"#
    );
    assert_eq!(
        store.get(&scope, unordered.digest()).await.unwrap().bytes,
        br#"{"a":[3,2,1],"b":1}"#
    );
}

#[tokio::test]
async fn application_json_rejects_invalid_and_non_i_json_without_publication() {
    let root = TestRoot::new("invalid-json");
    let store = open_store(root.path());
    let scope = scope();

    for bytes in [
        br#"{"unterminated":"#.as_slice(),
        br#"{"duplicate":1,"duplicate":2}"#.as_slice(),
        br#"{"unsafe_integer":9007199254740993}"#.as_slice(),
        b"{\"invalid_utf8\":\"\xff\"}".as_slice(),
    ] {
        assert!(matches!(
            store.put(&scope, bytes, "application/json").await,
            Err(ObjectStoreError::InvalidField)
        ));
    }

    let mut objects = Vec::new();
    collect_files(&root.path().join("objects"), &mut objects);
    assert!(objects.is_empty());
}

#[tokio::test]
async fn puts_fsyncs_and_replays_without_rewriting() {
    let root = TestRoot::new("idempotent");
    let store = open_store(root.path());
    let scope = scope();
    let first = store
        .put(&scope, b"durable bytes", "text/plain")
        .await
        .unwrap();
    let second = store
        .publish_if_absent(&scope, b"durable bytes", "text/plain")
        .await
        .unwrap();

    assert_eq!(first.digest(), second.digest());
    assert_eq!(
        fs::read(object_file(root.path())).unwrap(),
        b"durable bytes"
    );
    assert_eq!(
        store
            .get(&scope, first.digest())
            .await
            .unwrap()
            .reference
            .media_type(),
        "text/plain"
    );
}

#[tokio::test]
async fn same_bytes_with_different_media_type_is_metadata_conflict() {
    let root = TestRoot::new("media-conflict");
    let store = open_store(root.path());
    let scope = scope();
    store
        .put(&scope, b"same bytes", "text/plain")
        .await
        .unwrap();

    let error = store
        .put(&scope, b"same bytes", "application/octet-stream")
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ObjectStoreError::ArtifactMetadataConflict(_)
    ));
    assert_eq!(fs::read(object_file(root.path())).unwrap(), b"same bytes");
}

#[tokio::test]
async fn existing_tampered_path_returns_conflict_and_preserves_original_bytes() {
    let root = TestRoot::new("conflict");
    let store = open_store(root.path());
    let scope = scope();
    store
        .put(&scope, b"original", "application/octet-stream")
        .await
        .unwrap();
    let path = object_file(root.path());
    fs::write(&path, b"tampered").unwrap();

    let error = store
        .put(&scope, b"original", "application/octet-stream")
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ObjectStoreError::ArtifactMetadataConflict(_)
    ));
    assert_eq!(fs::read(path).unwrap(), b"tampered");
}

#[tokio::test]
async fn crash_before_sidecar_link_leaves_no_visible_object() {
    let root = TestRoot::new("orphan-temp");
    let scope = scope();
    let store = open_store(root.path());
    let orphan_data = root.path().join("tmp").join("killed-before-sidecar-data");
    let orphan_media = root.path().join("tmp").join("killed-before-sidecar-media");
    fs::write(&orphan_data, b"orphan").unwrap();
    fs::write(&orphan_media, b"text/plain").unwrap();
    fs::File::open(&orphan_data).unwrap().sync_all().unwrap();
    fs::File::open(&orphan_media).unwrap().sync_all().unwrap();
    drop(store);

    let reopened = open_store(root.path());
    let unknown = Digest::new(format!("sha256:{}", "0".repeat(64))).unwrap();
    let error = reopened.get(&scope, &unknown).await.unwrap_err();
    assert_eq!(error.proof.error_class(), FailedReadClass::Missing);
    assert!(orphan_data.exists());
    assert!(orphan_media.exists());
    assert!(root
        .path()
        .join("objects")
        .read_dir()
        .unwrap()
        .next()
        .is_none());
}

#[tokio::test]
async fn crash_after_sidecar_before_data_keeps_object_invisible_and_type_locked() {
    let root = TestRoot::new("after-sidecar");
    let scope = scope();
    let store = open_store(root.path());
    let digest = store
        .put(&scope, b"sidecar first", "text/plain")
        .await
        .unwrap()
        .digest()
        .clone();
    let object = object_file(root.path());
    let media = media_file(root.path(), &object);
    fs::remove_file(&object).unwrap();
    drop(store);

    let reopened = open_store(root.path());
    let error = reopened.get(&scope, &digest).await.unwrap_err();
    assert_eq!(error.proof.error_class(), FailedReadClass::Missing);
    assert!(matches!(
        reopened
            .put(&scope, b"sidecar first", "application/octet-stream")
            .await,
        Err(ObjectStoreError::ArtifactMetadataConflict(_))
    ));
    assert!(!object.exists());
    assert_eq!(fs::read(&media).unwrap(), b"text/plain");

    reopened
        .put(&scope, b"sidecar first", "text/plain")
        .await
        .unwrap();
    assert_eq!(
        reopened.get(&scope, &digest).await.unwrap().bytes,
        b"sidecar first"
    );
}

#[tokio::test]
async fn data_with_missing_or_invalid_sidecar_is_never_repaired() {
    let root = TestRoot::new("data-without-sidecar");
    let scope = scope();
    let store = open_store(root.path());
    let digest = store
        .put(&scope, b"must stay incomplete", "text/plain")
        .await
        .unwrap()
        .digest()
        .clone();
    let object = object_file(root.path());
    let media = media_file(root.path(), &object);
    fs::remove_file(&media).unwrap();
    drop(store);

    let reopened = open_store(root.path());
    for media_type in ["text/plain", "application/octet-stream"] {
        assert!(matches!(
            reopened
                .put(&scope, b"must stay incomplete", media_type)
                .await,
            Err(ObjectStoreError::StorageUnavailable)
        ));
        assert!(
            !media.exists(),
            "a retry must not recreate missing metadata"
        );
        assert_eq!(fs::read(&object).unwrap(), b"must stay incomplete");
    }

    fs::write(&media, []).unwrap();
    for media_type in ["text/plain", "application/octet-stream"] {
        assert!(matches!(
            reopened
                .put(&scope, b"must stay incomplete", media_type)
                .await,
            Err(ObjectStoreError::StorageUnavailable)
        ));
        assert_eq!(fs::read(&media).unwrap(), b"");
        assert_eq!(fs::read(&object).unwrap(), b"must stay incomplete");
    }

    let mut temporary = Vec::new();
    collect_files(&root.path().join("tmp"), &mut temporary);
    assert!(
        temporary.is_empty(),
        "existing incomplete objects must fail before creating candidates"
    );
    let error = reopened.get(&scope, &digest).await.unwrap_err();
    assert_eq!(error.proof.error_class(), FailedReadClass::Missing);
}

#[tokio::test]
async fn crash_after_data_link_has_a_complete_readable_object() {
    let root = TestRoot::new("after-rename");
    let scope = scope();
    let store = open_store(root.path());
    let reference = store
        .put(&scope, b"already published", "text/plain")
        .await
        .unwrap();
    let digest = reference.digest().clone();
    drop(store);

    let reopened = open_store(root.path());
    let replay = reopened
        .put(&scope, b"already published", "text/plain")
        .await
        .unwrap();
    assert_eq!(replay.digest(), &digest);
    let read = reopened.get(&scope, &digest).await.unwrap();
    assert_eq!(read.bytes, b"already published");
    assert_eq!(read.reference.media_type(), "text/plain");
}

#[test]
fn concurrent_same_digest_writers_publish_one_intact_object() {
    let root = TestRoot::new("concurrent");
    let store = Arc::new(open_store(root.path()));
    let scope = scope();
    std::thread::scope(|threads| {
        let first_store = Arc::clone(&store);
        let first_scope = scope.clone();
        let first = threads
            .spawn(move || block_on(first_store.put(&first_scope, b"same digest", "text/plain")));
        let second_store = Arc::clone(&store);
        let second_scope = scope.clone();
        let second = threads
            .spawn(move || block_on(second_store.put(&second_scope, b"same digest", "text/plain")));
        assert!(first.join().unwrap().is_ok());
        assert!(second.join().unwrap().is_ok());
    });

    let digest = block_on(store.put(&scope, b"same digest", "text/plain"))
        .unwrap()
        .digest()
        .clone();
    assert_eq!(fs::read(object_file(root.path())).unwrap(), b"same digest");
    assert_eq!(
        block_on(store.get(&scope, &digest)).unwrap().bytes,
        b"same digest"
    );
}

#[tokio::test]
async fn bit_flipped_object_is_reported_as_digest_invalid() {
    let root = TestRoot::new("bit-flip");
    let store = open_store(root.path());
    let scope = scope();
    let digest = store
        .put(&scope, b"integrity check", "application/octet-stream")
        .await
        .unwrap()
        .digest()
        .clone();
    let path = object_file(root.path());
    let mut bytes = fs::read(&path).unwrap();
    bytes[0] ^= 1;
    fs::write(path, bytes).unwrap();

    let error = store.get(&scope, &digest).await.unwrap_err();
    assert_eq!(error.proof.error_class(), FailedReadClass::DigestInvalid);
}

#[tokio::test]
async fn truncated_object_is_reported_as_digest_invalid() {
    let root = TestRoot::new("truncated");
    let store = open_store(root.path());
    let scope = scope();
    let digest = store
        .put(
            &scope,
            b"this object must be longer than one byte",
            "application/octet-stream",
        )
        .await
        .unwrap()
        .digest()
        .clone();
    let path = object_file(root.path());
    fs::write(path, b"short").unwrap();

    let error = store.get(&scope, &digest).await.unwrap_err();
    assert_eq!(error.proof.error_class(), FailedReadClass::DigestInvalid);
}

#[tokio::test]
async fn unreadable_object_is_reported_with_store_minted_missing_proof() {
    let root = TestRoot::new("unreadable");
    let store = open_store(root.path());
    let scope = scope();
    let digest = store
        .put(
            &scope,
            b"readable before tampering",
            "application/octet-stream",
        )
        .await
        .unwrap()
        .digest()
        .clone();
    let path = object_file(root.path());
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();

    let error = store.get(&scope, &digest).await.unwrap_err();
    assert_eq!(error.proof.error_class(), FailedReadClass::Missing);
}

#[tokio::test]
async fn unknown_digest_is_missing_not_corruption_by_assertion() {
    let root = TestRoot::new("missing");
    let store = open_store(root.path());
    let unknown = Digest::new(format!("sha256:{}", "f".repeat(64))).unwrap();

    let error = store.get(&scope(), &unknown).await.unwrap_err();
    assert_eq!(error.proof.error_class(), FailedReadClass::Missing);
}
