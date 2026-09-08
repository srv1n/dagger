use super::*;
use crate::scope::ScopeAtom;

fn capability(bytes: Vec<u8>) -> VerifiedObjectRef {
    VerifiedObjectRef::new(
        ExecutionScope {
            tenant_id: ScopeAtom::new("tenant-a").unwrap(),
            namespace: ScopeAtom::new("prod").unwrap(),
        },
        Digest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
        bytes.len() as u64,
        "application/octet-stream".to_owned(),
        "tenant-a/prod/object".to_owned(),
        b"private-store-nonce".to_vec(),
        bytes,
    )
}

#[test]
fn capability_construction_keeps_the_owned_payload_allocation() {
    let bytes = vec![42; 1024 * 1024];
    let pointer = bytes.as_ptr();
    let reference = capability(bytes);
    assert_eq!(reference.verified_bytes().as_ptr(), pointer);
    assert_eq!(reference.verified_bytes().len(), 1024 * 1024);
}

#[test]
fn capability_clones_share_bytes_and_outlive_the_original() {
    let reference = capability(vec![42; 1024 * 1024]);
    let cloned = reference.clone();
    assert_eq!(cloned, reference);
    assert!(Arc::ptr_eq(&reference.verified_bytes, &cloned.verified_bytes));
    assert_eq!(
        reference.verified_bytes().as_ptr(),
        cloned.verified_bytes().as_ptr()
    );
    drop(reference);
    assert_eq!(cloned.verified_bytes(), vec![42; 1024 * 1024]);
}

#[test]
fn capability_equality_remains_content_based_and_scope_bound() {
    let first = capability(vec![1, 2, 3]);
    let second = capability(vec![1, 2, 3]);
    assert!(!Arc::ptr_eq(&first.verified_bytes, &second.verified_bytes));
    assert_eq!(first, second);
    assert_ne!(first, capability(vec![1, 2, 4]));

    let mut other_store = second.clone();
    other_store.store_instance_nonce.push(0);
    assert_ne!(first, other_store);

    let mut other_scope = second;
    other_scope.scope.tenant_id = ScopeAtom::new("tenant-b").unwrap();
    assert_ne!(first, other_scope);
}

#[test]
fn returned_bytes_remain_independent_of_the_verified_capability() {
    let mut object = VerifiedObject {
        reference: capability(vec![1, 2, 3]),
        bytes: vec![1, 2, 3],
    };
    let cloned = object.clone();
    object.bytes[0] = 9;
    assert_eq!(object.reference.verified_bytes(), &[1, 2, 3]);
    assert_eq!(cloned.reference.verified_bytes(), &[1, 2, 3]);
    assert_eq!(cloned.bytes, vec![1, 2, 3]);
}

#[test]
fn shared_capability_keeps_debug_redaction() {
    let reference = capability(b"private-payload".to_vec()).clone();
    let rendered = format!("{reference:?}");
    assert!(rendered.contains("verified_bytes: \"<redacted>\""));
    assert!(rendered.contains("store_instance_nonce: \"<redacted>\""));
    assert!(!rendered.contains("private-payload"));
    assert!(!rendered.contains("private-store-nonce"));
}

#[test]
fn shared_capabilities_remain_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<VerifiedObjectRef>();
    let reference = capability(vec![1, 2, 3]);
    let cloned = reference.clone();
    let worker = std::thread::spawn(move || {
        assert_eq!(cloned.verified_bytes(), &[1, 2, 3]);
        cloned
    });
    assert_eq!(worker.join().unwrap(), reference);
}
