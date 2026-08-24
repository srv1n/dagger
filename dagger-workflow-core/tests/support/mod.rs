use dagger_workflow_core::action::{
    fixtures::{fixture_schema, fixture_schema_digest, FixtureActions},
    ActionRegistry,
};
use dagger_workflow_core::approval::AuthenticatedPrincipal;
use dagger_workflow_core::artifact::ObjectStore;
use dagger_workflow_core::definition::{
    parse_yaml_definition, resolve_publication, validate_definition, ActionReference,
    ExtractedActionPin, NodeDefinition, PublicationResolver, PublicationSchemaDocument,
    PublishableDefinition, WorkflowDefinition,
};
use dagger_workflow_core::engine::TestClock;
use dagger_workflow_core::ids::{Digest, Id, Version};
use dagger_workflow_core::memory::{InMemoryObjectStore, InMemoryStore};
use dagger_workflow_core::scope::ExecutionScope;
use dagger_workflow_core::store::{
    CreateDefinition, PublishRevision, ResolvedActionSchemas, WorkflowStore,
};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;

pub fn hash(bytes: &[u8]) -> Digest {
    Digest::new(format!(
        "sha256:{}",
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    ))
    .unwrap()
}

pub fn principal(scope: &ExecutionScope, principal_id: &str) -> AuthenticatedPrincipal {
    AuthenticatedPrincipal::mint(
        scope.clone(),
        principal_id.to_owned(),
        Vec::new(),
        hash(principal_id.as_bytes()),
    )
    .unwrap()
}

struct FixtureResolver {
    fixtures: FixtureActions,
}

impl PublicationResolver for FixtureResolver {
    fn schema_document(&self, digest: &Digest) -> Option<PublicationSchemaDocument> {
        (*digest == fixture_schema_digest()).then(|| PublicationSchemaDocument {
            digest: digest.clone(),
            value: fixture_schema(),
        })
    }

    fn artifact_exists(&self, _: &Id, _: &Digest, _: &str) -> bool {
        true
    }

    fn action_pin_available(&self, pin: &ExtractedActionPin) -> bool {
        self.fixtures
            .registry()
            .resolve(&pin.name)
            .is_some_and(|action| {
                let descriptor = action.descriptor();
                descriptor.contract_version == pin.contract_version
                    && descriptor.input_schema_digest == pin.input_schema_digest
                    && descriptor.output_schema_digest == pin.output_schema_digest
                    && descriptor.implementation_compatibility_digest
                        == pin.compatible_implementation_requirement
            })
    }
}

fn repin_action(action: &mut ActionReference, fixtures: &FixtureActions) {
    let descriptor = fixtures
        .registry()
        .resolve(&action.name)
        .unwrap_or_else(|| panic!("reference action {} is not registered", action.name))
        .descriptor()
        .clone();
    action.contract_version = descriptor.contract_version;
    action.input_schema_digest = descriptor.input_schema_digest;
    action.output_schema_digest = descriptor.output_schema_digest;
    action.compatible_implementation_requirement = descriptor.implementation_compatibility_digest;
}

/// Loads the reference YAML and explicitly replaces only its fixture placeholders with the exact
/// contracts advertised by the registered fixture actions. The YAML itself stays immutable.
pub fn repin_legal_research_reference(fixtures: &FixtureActions) -> PublishableDefinition {
    let path = format!(
        "{}/../docs/reference_workflows/legal_research.yaml",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut definition = parse_yaml_definition(&std::fs::read_to_string(path).unwrap()).unwrap();
    definition.run_input_schema_digest = fixture_schema_digest();
    definition.run_output_schema_digest = fixture_schema_digest();
    for node in &mut definition.nodes {
        match node {
            NodeDefinition::Action { action, .. } | NodeDefinition::Map { action, .. } => {
                repin_action(action, fixtures)
            }
            _ => {}
        }
    }
    resolve_publication(
        validate_definition(&definition).unwrap(),
        &FixtureResolver {
            fixtures: fixtures.clone(),
        },
    )
    .expect("explicit fixture repinning must still pass full publication verification")
}

pub struct PublishedReference {
    pub definition: WorkflowDefinition,
    pub revision_hash: Digest,
}

/// Publishes the explicitly repinned reference definition with durable schema objects whose
/// canonical bytes are verified against every exact fixture pin.
pub async fn publish_legal_research_reference(
    store: &InMemoryStore<TestClock>,
    objects: &InMemoryObjectStore<TestClock>,
    scope: &ExecutionScope,
    fixtures: &FixtureActions,
) -> PublishedReference {
    let publishable = repin_legal_research_reference(fixtures);
    let definition = publishable.definition.clone();
    let schema_bytes = serde_jcs::to_vec(&fixture_schema()).unwrap();
    let schema = objects
        .put(scope, &schema_bytes, "application/json")
        .await
        .unwrap();
    assert_eq!(schema.digest(), &fixture_schema_digest());
    let canonical = objects
        .put(
            scope,
            &serde_jcs::to_vec(&definition).unwrap(),
            "application/json",
        )
        .await
        .unwrap();
    let publishing_principal = principal(scope, "w5-publisher");
    store
        .create_definition(
            scope,
            CreateDefinition {
                definition_id: definition.definition_id.clone(),
                display_name: definition.name.clone(),
                description: definition.description.clone(),
                principal: publishing_principal.clone(),
            },
        )
        .await
        .unwrap();
    let resolved_action_schema_objects = definition
        .nodes
        .iter()
        .filter_map(|node| match node {
            NodeDefinition::Action { id, .. } => Some(id.as_str().to_owned()),
            NodeDefinition::Map { id, .. } => Some(format!("{}/map_action", id.as_str())),
            _ => None,
        })
        .map(|location| {
            (
                location,
                ResolvedActionSchemas {
                    input_schema: schema.clone(),
                    output_schema: schema.clone(),
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    store
        .publish_revision(
            scope,
            PublishRevision {
                definition_id: definition.definition_id.clone(),
                expected_definition_version: Version(1),
                canonical_definition: canonical.clone(),
                run_input_schema: schema.clone(),
                run_output_schema: schema,
                resolved_action_schema_objects,
                parsed_revision: publishable,
                principal: publishing_principal,
            },
        )
        .await
        .unwrap();
    PublishedReference {
        definition,
        revision_hash: canonical.digest().clone(),
    }
}
