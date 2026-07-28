use dagger_workflow_core::ids::{
    edge_id, idempotency_key, map_child_id, map_child_idempotency_key, Digest, Id,
};
use dagger_workflow_core::scope::{ExecutionScope, ScopeAtom};

fn scope() -> ExecutionScope {
    ExecutionScope {
        tenant_id: ScopeAtom::new("tenant-a").unwrap(),
        namespace: ScopeAtom::new("prod").unwrap(),
    }
}

#[test]
fn deterministic_derivations_are_stable_and_scope_bound() {
    let digest = Digest(
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
    );
    let run = Id("run-1".to_owned());
    let parent = Id("map".to_owned());
    let child = map_child_id(&run, &parent, 7, &digest);
    assert_eq!(
        child.0,
        "mapchild_9a18dfbcfd28671b31f9ec6e208373bc49ddf1144cdb129733e7edf00e65a125"
    );
    assert_eq!(
        edge_id(&digest, &parent, "next/0", &child).0,
        "edge_40a5e2ca19d5575ca62515c35bb64bb6cecb8e5ad467b598116749006420dbd4"
    );
    assert_eq!(
        idempotency_key(&scope(), &run, &child),
        "dwf-idem-v1:2488aab5e1247c04ef6625fa864b4f21588b53c04958d8159b9ad9dbcabad8cf"
    );
    assert_eq!(
        map_child_idempotency_key(&scope(), &run, &child, &parent, 7, &digest),
        "dwf-idem-v1:f89f16f4f1cecab1442121b1c88f9c3e425d058481c9cc31b6f3ef016d6cf61b"
    );
    assert_eq!(child, map_child_id(&run, &parent, 7, &digest));
    assert_ne!(child, map_child_id(&run, &parent, 8, &digest));
    assert_eq!(
        edge_id(&digest, &parent, "next/0", &child),
        edge_id(&digest, &parent, "next/0", &child)
    );
    assert_ne!(
        idempotency_key(&scope(), &run, &child),
        map_child_idempotency_key(&scope(), &run, &child, &parent, 7, &digest)
    );
}
