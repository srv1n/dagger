//! Differential checks against the buffered v1 identity encoding.
//! Keep this oracle independent of the production streaming helpers.

use dagger_workflow_core::artifact::ArtifactKind;
use dagger_workflow_core::ids::{
    artifact_ref_id, edge_id, idempotency_key, map_child_id, map_child_idempotency_key,
    map_expansion_digest, ArtifactRefIdentity, Digest, Id, MapChildIdentity,
};
use dagger_workflow_core::scope::{ExecutionScope, ScopeAtom};
use sha2::{Digest as _, Sha256};

fn scope(tenant: &str) -> ExecutionScope {
    ExecutionScope {
        tenant_id: ScopeAtom::new(tenant).unwrap(),
        namespace: ScopeAtom::new("prod").unwrap(),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn append_field(buffer: &mut Vec<u8>, value: &[u8]) {
    buffer.extend_from_slice(&(value.len() as u64).to_be_bytes());
    buffer.extend_from_slice(value);
}

fn encoded_hash(prefix: &str, bytes: &[u8]) -> String {
    format!("{prefix}{}", hex(&Sha256::digest(bytes)))
}

fn buffered_hash(prefix: &str, fields: &[&[u8]]) -> String {
    let mut buffer = Vec::new();
    for field in fields {
        append_field(&mut buffer, field);
    }
    encoded_hash(prefix, &buffer)
}

#[test]
fn identifiers_match_buffered_v1_at_field_and_integer_boundaries() {
    let texts = ["a", "ab", "a\0b", "雪/🦀", &"x".repeat(128)];
    for (case, text) in texts.iter().enumerate() {
        let run = Id::new(*text).unwrap();
        let node = Id::new(text.chars().rev().collect::<String>()).unwrap();
        let parent = Id::new("map-parent").unwrap();
        let raw: [u8; 32] = std::array::from_fn(|index| (index * 7 + case) as u8);
        let digest = Digest::new(format!("sha256:{}", hex(&raw))).unwrap();
        for tenant in ["tenant-a", "tenant-b"] {
            let scope = scope(tenant);
            let fields: [&[u8]; 5] = [
                b"dagger-idem-v1",
                scope.tenant_id.as_str().as_bytes(),
                scope.namespace.as_str().as_bytes(),
                run.as_str().as_bytes(),
                node.as_str().as_bytes(),
            ];
            assert_eq!(
                idempotency_key(&scope, &run, &node),
                buffered_hash("dwf-idem-v1:", &fields)
            );
            for index in [0_u32, 1, 255, 256, u32::MAX] {
                let mut map_fields = fields.to_vec();
                let index_bytes = index.to_be_bytes();
                map_fields.extend_from_slice(&[
                    b"map-child",
                    parent.as_str().as_bytes(),
                    &index_bytes,
                    digest.as_str().as_bytes(),
                ]);
                assert_eq!(
                    map_child_idempotency_key(&scope, &run, &node, &parent, index, &digest),
                    buffered_hash("dwf-idem-v1:", &map_fields)
                );

                let mut child_bytes = Vec::new();
                append_field(&mut child_bytes, b"dagger-map-child-v1");
                append_field(&mut child_bytes, run.as_str().as_bytes());
                append_field(&mut child_bytes, parent.as_str().as_bytes());
                // Unlike the other derivations, these two fields have no prefix.
                child_bytes.extend_from_slice(&index_bytes);
                child_bytes.extend_from_slice(&raw);
                assert_eq!(
                    map_child_id(&run, &parent, index, &digest).as_str(),
                    encoded_hash("mapchild_", &child_bytes)
                );
            }
        }
        for label in ["", "next/0", "a\0b", "雪"] {
            assert_eq!(
                edge_id(&digest, &run, label, &node).as_str(),
                buffered_hash(
                    "edge_",
                    &[
                        b"dagger-edge-v1",
                        &raw,
                        run.as_str().as_bytes(),
                        label.as_bytes(),
                        node.as_str().as_bytes(),
                    ],
                )
            );
        }
    }
}

#[test]
fn artifacts_match_v1_for_every_kind_and_optional_field_combination() {
    let kinds = [
        (ArtifactKind::RunInput, "RunInput"),
        (ArtifactKind::SchemaDocument, "SchemaDocument"),
        (ArtifactKind::Definition, "Definition"),
        (ArtifactKind::NodeOutput, "NodeOutput"),
        (ArtifactKind::ActionInvocationInput, "ActionInvocationInput"),
        (ArtifactKind::ActionArtifact, "ActionArtifact"),
        (ArtifactKind::Diagnostics, "Diagnostics"),
        (ArtifactKind::CompatibilityEvidence, "CompatibilityEvidence"),
        (ArtifactKind::ChoiceInput, "ChoiceInput"),
        (ArtifactKind::MapInput, "MapInput"),
        (ArtifactKind::MapAggregate, "MapAggregate"),
        (ArtifactKind::ApprovalRequest, "ApprovalRequest"),
        (ArtifactKind::ApprovalDecisionPayload, "ApprovalDecisionPayload"),
    ];
    let run = Id::new("run\0雪").unwrap();
    let node = Id::new("node").unwrap();
    let attempt = Id::new("attempt").unwrap();
    let raw = [0xab; 32];
    let digest = Digest::new(format!("sha256:{}", hex(&raw))).unwrap();
    for tenant in ["tenant-a", "tenant-b"] {
        let scope = scope(tenant);
        for (kind, name) in kinds {
            for mask in 0..8 {
                let producer_run_id = (mask & 1 != 0).then_some(&run);
                let producer_node_id = (mask & 2 != 0).then_some(&node);
                let producer_attempt_id = (mask & 4 != 0).then_some(&attempt);
                for ordinal in [0_u32, u32::MAX] {
                    let identity = ArtifactRefIdentity {
                        scope: &scope,
                        digest: &digest,
                        kind,
                        producer_run_id,
                        producer_node_id,
                        producer_attempt_id,
                        ordinal,
                    };
                    let optional_bytes = |id: Option<&Id>| {
                        id.map(|id| id.as_str().as_bytes().to_vec())
                            .unwrap_or_default()
                    };
                    assert_eq!(
                        artifact_ref_id(identity).as_str(),
                        buffered_hash(
                            "artifact_",
                            &[
                                b"dagger-artifact-ref-v1",
                                scope.tenant_id.as_str().as_bytes(),
                                scope.namespace.as_str().as_bytes(),
                                &raw,
                                name.as_bytes(),
                                &optional_bytes(producer_run_id),
                                &optional_bytes(producer_node_id),
                                &optional_bytes(producer_attempt_id),
                                &ordinal.to_be_bytes(),
                            ],
                        )
                    );
                }
            }
        }
    }
}

fn buffered_expansion(children: &[MapChildIdentity]) -> String {
    let mut bytes = Vec::new();
    append_field(&mut bytes, b"dagger-map-expansion-v1");
    for child in children {
        append_field(&mut bytes, &child.item_index.to_be_bytes());
        let text = &child.item_digest.as_str()[7..];
        let raw: Vec<u8> = (0..32)
            .map(|index| u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).unwrap())
            .collect();
        append_field(&mut bytes, &raw);
        append_field(&mut bytes, child.child_id.as_str().as_bytes());
    }
    encoded_hash("sha256:", &bytes)
}

#[test]
fn map_expansion_matches_v1_for_empty_large_and_reordered_inputs() {
    let children: Vec<_> = (0_u32..1024)
        .map(|index| MapChildIdentity {
            item_index: index,
            item_digest: Digest::new(format!(
                "sha256:{}",
                hex(&Sha256::digest(index.to_be_bytes()))
            ))
            .unwrap(),
            child_id: Id::new(format!("child-{index}")).unwrap(),
        })
        .collect();
    for count in [0, 1, 2, 31, 32, 255, 256, 1024] {
        assert_eq!(
            map_expansion_digest(&children[..count]).as_str(),
            buffered_expansion(&children[..count])
        );
    }
    let reversed: Vec<_> = children.iter().cloned().rev().collect();
    assert_eq!(
        map_expansion_digest(&reversed).as_str(),
        buffered_expansion(&reversed)
    );
    assert_ne!(map_expansion_digest(&children), map_expansion_digest(&reversed));
}
