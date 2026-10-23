//! Strict JSON/YAML manifests for host-bound action registration.
//!
//! A manifest has an `actions` array. Each entry contains `name`,
//! `contract_version`, `input_schema`, `output_schema`, and
//! `implementation_digest`. `memoize` defaults to false and
//! `required_capabilities` defaults to an empty array (`capability_tags` is an
//! alias). The host factory receives the exact computed descriptor and supplies
//! a matching executable action.

use super::schema::{opaque_schema_bundle, SchemaValidationMode};
use super::{
    ActionContext, ActionDescriptor, ActionOutcome, ActionRegistrationError, ActionRegistry,
    InMemoryActionRegistry, WorkflowAction,
};
use crate::artifact::{ObjectStore, ObjectStoreError, VerifiedObjectRef};
use crate::definition::{
    extract_action_pins, revision_hash, ActionReference, ExtractedActionPin, NoDuplicateJson,
    PublicationResolver, PublicationSchemaDocument, WorkflowDefinition,
};
use crate::ids::{Digest, Id};
use crate::scope::ExecutionScope;
use crate::store::ResolvedActionSchemas;
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// A rejected manifest, binding, or schema publication.
#[derive(Debug, thiserror::Error)]
pub enum ActionManifestError {
    /// The source is malformed, exceeds parser limits, or has unknown fields.
    #[error("invalid action manifest: {0}")]
    Parse(String),
    /// A descriptor field or schema violates the publication contract.
    #[error("invalid manifest field {path}: {message}")]
    InvalidField {
        /// Location of the invalid field.
        path: String,
        /// Reason for rejection.
        message: String,
    },
    /// The same action appears more than once.
    #[error("manifest repeats action {name:?}")]
    DuplicateName {
        /// Repeated registry name.
        name: String,
    },
    /// The host returned an executable with a different exact descriptor.
    #[error("factory descriptor does not match manifest action {name:?}")]
    DescriptorMismatch {
        /// Manifest action being bound.
        name: String,
    },
    /// The host could not bind an entry to an executable.
    #[error("factory could not bind action {name:?}: {message}")]
    Factory {
        /// Manifest action being bound.
        name: String,
        /// Host-provided failure explanation.
        message: String,
    },
    /// The batch could not be installed in the registry.
    #[error(transparent)]
    Registration(#[from] ActionRegistrationError),
    /// A schema object could not be durably published.
    #[error(transparent)]
    ObjectStore(#[from] ObjectStoreError),
    /// A definition refers to a schema absent from this manifest's materials.
    #[error("manifest schema object {digest:?} is unavailable")]
    MissingSchema {
        /// Missing pinned schema digest.
        digest: Digest,
    },
}

/// One validated immutable manifest entry.
#[derive(Clone, Debug, PartialEq)]
pub struct ManifestAction {
    descriptor: ActionDescriptor,
    input_schema: Value,
    output_schema: Value,
    memoize: bool,
    required_capabilities: Vec<String>,
}

impl ManifestAction {
    /// Returns the exact descriptor the host factory must implement.
    pub fn descriptor(&self) -> &ActionDescriptor {
        &self.descriptor
    }

    /// Returns the input schema document before canonical serialization.
    pub fn input_schema(&self) -> &Value {
        &self.input_schema
    }

    /// Returns the output schema document before canonical serialization.
    pub fn output_schema(&self) -> &Value {
        &self.output_schema
    }

    /// Reports whether this entry opts into deterministic cross-run reuse.
    pub fn memoize(&self) -> bool {
        self.memoize
    }

    /// Returns the host-defined capability declarations for this action.
    pub fn required_capabilities(&self) -> &[String] {
        &self.required_capabilities
    }

    /// Builds an exact action reference for an authored workflow definition.
    pub fn action_reference(&self) -> ActionReference {
        ActionReference {
            name: self.descriptor.name.clone(),
            contract_version: self.descriptor.contract_version.clone(),
            input_schema_digest: self.descriptor.input_schema_digest.clone(),
            output_schema_digest: self.descriptor.output_schema_digest.clone(),
            compatible_implementation_requirement: self
                .descriptor
                .implementation_compatibility_digest
                .clone(),
        }
    }
}

/// A validated manifest whose schema digests use publication's RFC 8785 bytes.
#[derive(Clone, Debug, PartialEq)]
pub struct ActionManifest {
    actions: Vec<ManifestAction>,
    schemas: BTreeMap<Digest, Value>,
    run_input_schema_digest: Option<Digest>,
    run_output_schema_digest: Option<Digest>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    actions: Vec<RawAction>,
    #[serde(default)]
    schema_documents: BTreeMap<String, Value>,
    #[serde(default)]
    run_input_schema: Option<Value>,
    #[serde(default)]
    run_output_schema: Option<Value>,
    #[serde(default)]
    run_input_schema_digest: Option<Digest>,
    #[serde(default)]
    run_output_schema_digest: Option<Digest>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAction {
    name: String,
    contract_version: String,
    input_schema: Value,
    output_schema: Value,
    #[serde(default)]
    input_schema_digest: Option<Digest>,
    #[serde(default)]
    output_schema_digest: Option<Digest>,
    implementation_digest: Digest,
    #[serde(default)]
    memoize: bool,
    #[serde(default, alias = "capability_tags")]
    required_capabilities: Vec<String>,
}

impl ActionManifest {
    /// Parses JSON, rejecting duplicate keys including those inside schemas.
    pub fn from_json(input: &str) -> Result<Self, ActionManifestError> {
        Self::from_json_with_schema_mode(input, SchemaValidationMode::SupportedSubset)
    }

    /// Parses an engine manifest with an explicit schema enforcement choice.
    ///
    /// Opaque mode pins unchanged schema documents with their closed offline
    /// `schema_documents` dependencies. The host action validates all payloads.
    pub fn from_json_with_schema_mode(
        input: &str,
        mode: SchemaValidationMode,
    ) -> Result<Self, ActionManifestError> {
        check_input_size(input)?;
        let value = serde_json::from_str::<NoDuplicateJson>(input)
            .map_err(|error| ActionManifestError::Parse(error.to_string()))?
            .0;
        let raw = serde_json::from_value(value)
            .map_err(|error| ActionManifestError::Parse(error.to_string()))?;
        Self::validate(raw, mode)
    }

    /// Parses bounded single-document YAML, rejecting duplicate and merge keys.
    pub fn from_yaml(input: &str) -> Result<Self, ActionManifestError> {
        Self::from_yaml_with_schema_mode(input, SchemaValidationMode::SupportedSubset)
    }

    /// Parses bounded YAML with the same explicit schema choice as JSON.
    pub fn from_yaml_with_schema_mode(
        input: &str,
        mode: SchemaValidationMode,
    ) -> Result<Self, ActionManifestError> {
        check_input_size(input)?;
        let options = serde_saphyr::options! {
            budget: serde_saphyr::budget! { max_documents: 1, max_events: 100_000, max_nodes: 20_000, max_depth: 64, max_aliases: 128, max_anchors: 128, max_total_scalar_bytes: 4 * 1024 * 1024 },
            duplicate_keys: serde_saphyr::DuplicateKeyPolicy::Error,
            merge_keys: serde_saphyr::MergeKeyPolicy::Error,
            strict_booleans: true,
            alias_limits: serde_saphyr::alias_limits! { max_total_replayed_events: 20_000, max_replay_stack_depth: 32, max_alias_expansions_per_anchor: 32 },
        };
        let raw = serde_saphyr::from_str_with_options(input, options)
            .map_err(|error| ActionManifestError::Parse(error.to_string()))?;
        Self::validate(raw, mode)
    }

    fn validate(raw: RawManifest, mode: SchemaValidationMode) -> Result<Self, ActionManifestError> {
        let raw_manifest_dependencies = raw.schema_documents;
        let mut names = BTreeSet::new();
        let mut actions = Vec::with_capacity(raw.actions.len());
        let mut schemas = BTreeMap::new();
        for (index, raw) in raw.actions.into_iter().enumerate() {
            let path = format!("/actions/{index}");
            if raw.name.is_empty() || raw.name.len() > 200 {
                return Err(invalid(
                    format!("{path}/name"),
                    "requires 1 through 200 bytes",
                ));
            }
            if !names.insert(raw.name.clone()) {
                return Err(ActionManifestError::DuplicateName { name: raw.name });
            }
            if raw.contract_version.is_empty() || raw.contract_version.len() > 64 {
                return Err(invalid(
                    format!("{path}/contract_version"),
                    "requires 1 through 64 bytes",
                ));
            }
            let mut capabilities = BTreeSet::new();
            for tag in &raw.required_capabilities {
                if tag.is_empty() || tag.len() > 200 || !capabilities.insert(tag) {
                    return Err(invalid(
                        format!("{path}/required_capabilities"),
                        "tags must be unique and contain 1 through 200 bytes",
                    ));
                }
            }
            let input_schema_digest = register_schema(
                &mut schemas,
                &raw.input_schema,
                raw.input_schema_digest.as_ref(),
                &raw_manifest_dependencies,
                mode,
                format!("{path}/input_schema"),
            )?;
            let output_schema_digest = register_schema(
                &mut schemas,
                &raw.output_schema,
                raw.output_schema_digest.as_ref(),
                &raw_manifest_dependencies,
                mode,
                format!("{path}/output_schema"),
            )?;
            actions.push(ManifestAction {
                descriptor: ActionDescriptor {
                    name: raw.name,
                    contract_version: raw.contract_version,
                    input_schema_digest,
                    output_schema_digest,
                    implementation_compatibility_digest: raw.implementation_digest,
                },
                input_schema: raw.input_schema,
                output_schema: raw.output_schema,
                memoize: raw.memoize,
                required_capabilities: raw.required_capabilities,
            });
        }
        let root_digest = |schema: Option<&Value>,
                           declared: Option<&Digest>,
                           path: &str,
                           schemas: &mut BTreeMap<Digest, Value>| {
            match schema {
                Some(schema) => register_schema(
                    schemas,
                    schema,
                    declared,
                    &raw_manifest_dependencies,
                    mode,
                    path,
                )
                .map(Some),
                None if declared.is_some() => Err(invalid(
                    path,
                    "a declared digest requires its schema document",
                )),
                None => Ok(None),
            }
        };
        let run_input_schema_digest = root_digest(
            raw.run_input_schema.as_ref(),
            raw.run_input_schema_digest.as_ref(),
            "/run_input_schema",
            &mut schemas,
        )?;
        let run_output_schema_digest = root_digest(
            raw.run_output_schema.as_ref(),
            raw.run_output_schema_digest.as_ref(),
            "/run_output_schema",
            &mut schemas,
        )?;
        Ok(Self {
            actions,
            schemas,
            run_input_schema_digest,
            run_output_schema_digest,
        })
    }

    /// Returns the validated entries in authored order.
    pub fn actions(&self) -> &[ManifestAction] {
        &self.actions
    }

    /// Returns the optional manifest root input schema pin, including bundle identity.
    pub fn run_input_schema_digest(&self) -> Option<&Digest> {
        self.run_input_schema_digest.as_ref()
    }

    /// Returns the optional manifest root output schema pin, including bundle identity.
    pub fn run_output_schema_digest(&self) -> Option<&Digest> {
        self.run_output_schema_digest.as_ref()
    }

    /// Binds every entry with the host factory and atomically installs the batch.
    ///
    /// Each returned action must advertise the entry's exact descriptor. A host
    /// may share command execution code between these distinct implementations.
    /// Factory failures, mismatched descriptors, and duplicate registry names
    /// leave the registry unchanged. Host factory side effects remain host-owned.
    pub fn register(
        &self,
        registry: &InMemoryActionRegistry,
        mut factory: impl FnMut(&ManifestAction) -> Result<Arc<dyn WorkflowAction>, String>,
    ) -> Result<(), ActionManifestError> {
        let mut batch: Vec<Arc<dyn WorkflowAction>> = Vec::with_capacity(self.actions.len());
        for entry in &self.actions {
            let implementation =
                factory(entry).map_err(|message| ActionManifestError::Factory {
                    name: entry.descriptor.name.clone(),
                    message,
                })?;
            if implementation.descriptor() != &entry.descriptor {
                return Err(ActionManifestError::DescriptorMismatch {
                    name: entry.descriptor.name.clone(),
                });
            }
            batch.push(Arc::new(ManifestBoundAction {
                implementation,
                descriptor: entry.descriptor.clone(),
                memoize: entry.memoize,
                required_capabilities: entry.required_capabilities.clone(),
            }));
        }
        registry.register_many(batch)?;
        Ok(())
    }

    /// Resolves this manifest's schema documents and exact registered pins.
    ///
    /// The resolver declines literal artifact references. Hosts can compose it
    /// with their root-schema and artifact resolver when a definition needs them.
    pub fn publication_resolver<'a>(
        &'a self,
        registry: &'a dyn ActionRegistry,
    ) -> ManifestPublicationResolver<'a> {
        ManifestPublicationResolver {
            manifest: self,
            registry,
        }
    }

    /// Durably publishes each distinct canonical schema document once.
    pub async fn publish_schemas(
        &self,
        objects: &impl ObjectStore,
        scope: &ExecutionScope,
    ) -> Result<ManifestSchemaObjects, ActionManifestError> {
        let mut published = BTreeMap::new();
        for (digest, value) in &self.schemas {
            let bytes =
                serde_jcs::to_vec(value).map_err(|error| invalid("/actions", error.to_string()))?;
            let object = objects.put(scope, &bytes, "application/json").await?;
            if object.digest() != digest || object.scope() != scope {
                return Err(ObjectStoreError::InvalidField.into());
            }
            published.insert(digest.clone(), object);
        }
        Ok(ManifestSchemaObjects { objects: published })
    }
}

/// Publication-stage resolver backed by a manifest and a live action registry.
pub struct ManifestPublicationResolver<'a> {
    manifest: &'a ActionManifest,
    registry: &'a dyn ActionRegistry,
}

impl PublicationResolver for ManifestPublicationResolver<'_> {
    fn schema_document(&self, digest: &Digest) -> Option<PublicationSchemaDocument> {
        self.manifest
            .schemas
            .get(digest)
            .map(|value| PublicationSchemaDocument {
                digest: digest.clone(),
                value: value.clone(),
            })
    }

    fn artifact_exists(&self, _: &Id, _: &Digest, _: &str) -> bool {
        false
    }

    fn action_pin_available(&self, pin: &ExtractedActionPin) -> bool {
        self.registry.resolve(&pin.name).is_some_and(|action| {
            let descriptor = action.descriptor();
            descriptor.name == pin.name
                && descriptor.contract_version == pin.contract_version
                && descriptor.input_schema_digest == pin.input_schema_digest
                && descriptor.output_schema_digest == pin.output_schema_digest
                && descriptor.implementation_compatibility_digest
                    == pin.compatible_implementation_requirement
        })
    }
}

/// Verified schema objects ready for [`crate::store::PublishRevision`].
#[derive(Clone, Debug)]
pub struct ManifestSchemaObjects {
    objects: BTreeMap<Digest, VerifiedObjectRef>,
}

impl ManifestSchemaObjects {
    /// Returns a published schema capability by its computed digest.
    pub fn schema(&self, digest: &Digest) -> Option<&VerifiedObjectRef> {
        self.objects.get(digest)
    }

    /// Resolves action schemas by the definition's node or Map-action location.
    pub fn resolve_action_schemas(
        &self,
        definition: &WorkflowDefinition,
    ) -> Result<BTreeMap<String, ResolvedActionSchemas>, ActionManifestError> {
        let mut resolved = BTreeMap::new();
        for pin in extract_action_pins(definition) {
            let schema = |digest: &Digest| {
                self.schema(digest)
                    .cloned()
                    .ok_or_else(|| ActionManifestError::MissingSchema {
                        digest: digest.clone(),
                    })
            };
            resolved.insert(
                pin.reference_location,
                ResolvedActionSchemas {
                    input_schema: schema(&pin.input_schema_digest)?,
                    output_schema: schema(&pin.output_schema_digest)?,
                },
            );
        }
        Ok(resolved)
    }
}

struct ManifestBoundAction {
    implementation: Arc<dyn WorkflowAction>,
    descriptor: ActionDescriptor,
    memoize: bool,
    required_capabilities: Vec<String>,
}

impl WorkflowAction for ManifestBoundAction {
    fn descriptor(&self) -> &ActionDescriptor {
        &self.descriptor
    }

    fn memoize(&self) -> bool {
        self.memoize
    }

    fn required_capabilities(&self) -> &[String] {
        &self.required_capabilities
    }

    fn invoke<'a>(
        &'a self,
        context: ActionContext,
        canonical_bound_input: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = ActionOutcome> + Send + 'a>> {
        self.implementation.invoke(context, canonical_bound_input)
    }
}

fn check_input_size(input: &str) -> Result<(), ActionManifestError> {
    if input.len() > 4 * 1024 * 1024 {
        Err(ActionManifestError::Parse("input exceeds 4 MiB".to_owned()))
    } else {
        Ok(())
    }
}

fn register_schema(
    schemas: &mut BTreeMap<Digest, Value>,
    schema: &Value,
    declared: Option<&Digest>,
    dependencies: &BTreeMap<String, Value>,
    mode: SchemaValidationMode,
    path: impl Into<String>,
) -> Result<Digest, ActionManifestError> {
    let path = path.into();
    let source_bytes =
        serde_jcs::to_vec(schema).map_err(|error| invalid(&path, error.to_string()))?;
    if declared.is_some_and(|digest| digest != &revision_hash(&source_bytes)) {
        return Err(invalid(
            &path,
            "declared digest does not match the unchanged schema document",
        ));
    }
    let schema = match mode {
        SchemaValidationMode::SupportedSubset => {
            if schema.get("schema_format").is_some() {
                return Err(invalid(
                    &path,
                    "opaque bundles require explicit host opt-in",
                ));
            }
            schema.clone()
        }
        SchemaValidationMode::Opaque => opaque_schema_bundle(schema.clone(), dependencies)
            .map_err(|error| invalid(&path, error.to_string()))?,
    };
    let bytes = serde_jcs::to_vec(&schema).map_err(|error| invalid(&path, error.to_string()))?;
    crate::memory::validate_schema_document(&bytes)
        .map_err(|error| invalid(&path, error.to_string()))?;
    let digest = revision_hash(&bytes);
    schemas.insert(digest.clone(), schema);
    Ok(digest)
}

fn invalid(path: impl Into<String>, message: impl Into<String>) -> ActionManifestError {
    ActionManifestError::InvalidField {
        path: path.into(),
        message: message.into(),
    }
}
