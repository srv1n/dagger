use dagger_workflow_core::artifact::ArtifactKind;
use dagger_workflow_core::ids::{
    artifact_ref_id, edge_id, idempotency_key, map_child_id, map_child_idempotency_key,
    map_expansion_digest, ArtifactRefIdentity, Digest, Id, MapChildIdentity,
};
use dagger_workflow_core::scope::{ExecutionScope, ScopeAtom};

fn id(value: &str) -> Id {
    Id::new(value).unwrap()
}
fn digest(value: char) -> Digest {
    Digest::new(format!("sha256:{}", value.to_string().repeat(64))).unwrap()
}

fn scope() -> ExecutionScope {
    ExecutionScope {
        tenant_id: ScopeAtom::new("tenant-a").unwrap(),
        namespace: ScopeAtom::new("prod").unwrap(),
    }
}

#[test]
fn deterministic_derivations_pin_contract_bytes() {
    let digest = digest('a');
    let run = id("run-1");
    let parent = id("map");
    let child = map_child_id(&run, &parent, 7, &digest);
    assert_eq!(
        child.as_str(),
        "mapchild_cec9b9ce43c43c11c7c6ac440d8ab59357f616eddc3ccb85d141249a8d609b9f"
    );
    assert_eq!(
        edge_id(&digest, &parent, "next/0", &child).as_str(),
        "edge_9cbacd195aa959a014f85a91f0dcd1354442db26cde22824d699c9d2a696ded1"
    );
    assert_eq!(
        idempotency_key(&scope(), &run, &child),
        "dwf-idem-v1:5d8f83e8268ac3d77884d938794d8e82aaad2bfbc52297ec75a2e0a54b5eed91"
    );
    assert_eq!(
        map_child_idempotency_key(&scope(), &run, &child, &parent, 7, &digest),
        "dwf-idem-v1:78a8ecfb07670b1aaabb0bba29cb488c6bc153801950ce506552448ef1bfdfd5"
    );
    let scoped = scope();
    let attempt = id("attempt-1");
    let artifact = artifact_ref_id(ArtifactRefIdentity {
        scope: &scoped,
        digest: &digest,
        kind: ArtifactKind::ActionArtifact,
        producer_run_id: Some(&run),
        producer_node_id: Some(&parent),
        producer_attempt_id: Some(&attempt),
        ordinal: 3,
    });
    assert_eq!(
        artifact.as_str(),
        "artifact_9fcc896c6609685402aa68a624d300815241ed1b7f2f8861cfb6ea3e8f74f097"
    );
}

#[test]
fn tuple_permutations_change_every_derived_identifier() {
    let item_digest = digest('a');
    let other_digest = digest('b');
    let run = id("run-1");
    let other_run = id("run-2");
    let parent = id("map");
    let other_parent = id("map-2");
    let child = map_child_id(&run, &parent, 7, &item_digest);
    let other_child = map_child_id(&other_run, &other_parent, 8, &other_digest);
    let attempt = id("attempt-1");
    let artifact = |scope: &ExecutionScope, run: &Id, parent: &Id, digest: &Digest| {
        artifact_ref_id(ArtifactRefIdentity {
            scope,
            digest,
            kind: ArtifactKind::ActionArtifact,
            producer_run_id: Some(run),
            producer_node_id: Some(parent),
            producer_attempt_id: Some(&attempt),
            ordinal: 3,
        })
    };
    assert_ne!(
        edge_id(&item_digest, &parent, "next/0", &child),
        edge_id(&other_digest, &other_parent, "next/1", &other_child)
    );
    assert_ne!(
        artifact(&scope(), &run, &parent, &item_digest),
        artifact(&scope(), &other_run, &other_parent, &other_digest)
    );
    assert_ne!(
        idempotency_key(&scope(), &run, &child),
        idempotency_key(&scope(), &other_run, &other_child)
    );
    assert_ne!(
        map_child_id(&run, &parent, 7, &item_digest),
        map_child_id(&other_run, &other_parent, 8, &other_digest)
    );
    assert_ne!(
        map_child_idempotency_key(&scope(), &run, &child, &parent, 7, &item_digest),
        map_child_idempotency_key(
            &scope(),
            &other_run,
            &other_child,
            &other_parent,
            8,
            &other_digest
        )
    );
    assert_ne!(
        map_expansion_digest(&[MapChildIdentity {
            item_index: 7,
            item_digest: item_digest.clone(),
            child_id: child
        }]),
        map_expansion_digest(&[MapChildIdentity {
            item_index: 8,
            item_digest: other_digest,
            child_id: other_child
        }]),
    );
}

#[test]
fn equal_run_and_node_ids_are_scope_separated() {
    let run = id("run");
    let node = id("node");
    let scope_b = ExecutionScope {
        tenant_id: ScopeAtom::new("tenant-b").unwrap(),
        namespace: ScopeAtom::new("prod").unwrap(),
    };
    let key_a = idempotency_key(&scope(), &run, &node);
    let key_b = idempotency_key(&scope_b, &run, &node);
    assert_eq!(
        key_a,
        "dwf-idem-v1:cd74205c34ecc749d4b0f4b17ba59d43df647796346bc711e379416e68b6a9da"
    );
    assert_eq!(
        key_b,
        "dwf-idem-v1:982c5e8c42fea88e028ed6a1dbc774588db47c8bd2f4280eed30ba9608266a7a"
    );
    assert_ne!(key_a, key_b);
    let map_key_a =
        map_child_idempotency_key(&scope(), &run, &node, &id("parent"), 0, &digest('a'));
    let map_key_b =
        map_child_idempotency_key(&scope_b, &run, &node, &id("parent"), 0, &digest('a'));
    assert_ne!(map_key_a, map_key_b);
}

#[test]
fn identifiers_and_scope_atoms_reject_invalid_construction_and_deserialization() {
    assert!(Id::new("").is_err());
    assert!(Digest::new("sha256:not-hex").is_err());
    assert!(serde_json::from_str::<Id>("\"\"").is_err());
    assert!(serde_json::from_str::<Digest>(
        "\"sha256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\""
    )
    .is_err());
    assert!(serde_json::from_str::<ScopeAtom>("\"bad space\"").is_err());
}
