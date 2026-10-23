//! Explicit host-validated JSON Schema bundles with closed offline references.
//!
//! Original schema documents remain unchanged inside the canonical bundle. Dagger
//! pins the entire bundle and treats its payload shape as opaque. The host action
//! must validate inputs and outputs with its own JSON Schema 2020-12 validator.

use crate::store::StoreError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

const OPAQUE_FORMAT: &str = "dagger.opaque-schema-bundle/v1";

/// Schema enforcement selected explicitly by the registering host.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SchemaValidationMode {
    /// Dagger validates its existing supported subset at publication and runtime.
    #[default]
    SupportedSubset,
    /// Dagger pins a closed schema bundle; the action validates its payloads.
    Opaque,
}

/// An invalid or incomplete offline schema bundle.
#[derive(Debug, thiserror::Error)]
#[error("invalid opaque schema bundle: {0}")]
pub struct SchemaBundleError(pub String);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpaqueBundle {
    schema_format: String,
    validation: String,
    root_document: String,
    documents: BTreeMap<String, Value>,
}

/// Bundles an unchanged root schema with every referenced supplied document.
///
/// Dependencies are keyed by their URI or a stable local document name. `$id`
/// declarations provide additional URI lookup names. The bundle supports local
/// JSON Pointer fragments, anchors, relative document references, and recursive
/// references. It never reads files or fetches a URI. Unused supplied documents
/// do not affect the resulting digest. Payload validation remains the host's job.
pub fn opaque_schema_bundle(
    root: Value,
    dependencies: &BTreeMap<String, Value>,
) -> Result<Value, SchemaBundleError> {
    if dependencies.contains_key("root") {
        return Err(bundle_error("dependency name 'root' is reserved"));
    }
    let mut documents = dependencies.clone();
    documents.insert("root".to_owned(), root);
    let used = reference_closure(&documents)?;
    documents.retain(|name, _| used.contains(name));
    let value = serde_json::to_value(OpaqueBundle {
        schema_format: OPAQUE_FORMAT.to_owned(),
        validation: "host".to_owned(),
        root_document: "root".to_owned(),
        documents,
    })
    .map_err(|error| bundle_error(error.to_string()))?;
    if serde_jcs::to_vec(&value)
        .map_err(|error| bundle_error(error.to_string()))?
        .len()
        > 1024 * 1024
    {
        return Err(bundle_error("canonical bundle exceeds 1 MiB"));
    }
    Ok(value)
}

/// Reports whether a schema explicitly selects the opaque bundle format.
pub(crate) fn is_opaque_bundle(value: &Value) -> bool {
    value.get("schema_format").and_then(Value::as_str) == Some(OPAQUE_FORMAT)
}

/// Validates the exact opaque envelope and its closed reference set.
pub(crate) fn validate_opaque_bundle(value: &Value) -> Result<(), StoreError> {
    let bundle: OpaqueBundle =
        serde_json::from_value(value.clone()).map_err(|_| StoreError::SchemaSubsetUnsupported)?;
    if bundle.schema_format != OPAQUE_FORMAT
        || bundle.validation != "host"
        || bundle.root_document != "root"
        || !bundle.documents.contains_key("root")
        || serde_jcs::to_vec(value)
            .map_err(|_| StoreError::SchemaSubsetUnsupported)?
            .len()
            > 1024 * 1024
    {
        return Err(StoreError::SchemaSubsetUnsupported);
    }
    let used =
        reference_closure(&bundle.documents).map_err(|_| StoreError::SchemaSubsetUnsupported)?;
    if used.len() != bundle.documents.len() {
        return Err(StoreError::SchemaSubsetUnsupported);
    }
    Ok(())
}

type ResourceLocation = (String, String);

fn reference_closure(
    documents: &BTreeMap<String, Value>,
) -> Result<BTreeSet<String>, SchemaBundleError> {
    let mut resources = BTreeMap::<String, ResourceLocation>::new();
    let mut anchors = BTreeMap::<String, ResourceLocation>::new();
    let mut references = BTreeMap::<String, Vec<(String, String)>>::new();
    for (document_name, document) in documents {
        if document_name.is_empty() || document_name.contains('#') {
            return Err(bundle_error(
                "document names must be nonempty and have no fragment",
            ));
        }
        insert_location(
            &mut resources,
            document_name.clone(),
            (document_name.clone(), String::new()),
        )?;
        let mut visit = |schema: &Value, base: &str, path: &str| {
            if path.is_empty() || schema.get("$id").is_some() {
                insert_location(
                    &mut resources,
                    base.to_owned(),
                    (document_name.clone(), path.to_owned()),
                )?;
            }
            for keyword in ["$anchor", "$dynamicAnchor"] {
                if let Some(anchor) = schema.get(keyword) {
                    let anchor = anchor
                        .as_str()
                        .filter(|value| valid_anchor(value))
                        .ok_or_else(|| bundle_error("invalid schema anchor"))?;
                    insert_location(
                        &mut anchors,
                        format!("{base}#{anchor}"),
                        (document_name.clone(), path.to_owned()),
                    )?;
                }
            }
            for keyword in ["$ref", "$dynamicRef"] {
                if let Some(reference) = schema.get(keyword) {
                    let reference = reference
                        .as_str()
                        .ok_or_else(|| bundle_error("schema reference must be a string"))?;
                    references
                        .entry(document_name.clone())
                        .or_default()
                        .push((base.to_owned(), reference.to_owned()));
                }
            }
            Ok(())
        };
        walk_schema(document, document_name, "", 0, &mut visit)?;
    }
    let mut used = BTreeSet::new();
    let mut pending = VecDeque::from(["root".to_owned()]);
    while let Some(name) = pending.pop_front() {
        if !used.insert(name.clone()) {
            continue;
        }
        if !documents.contains_key(&name) {
            return Err(bundle_error("root document is missing"));
        }
        for (base, reference) in references.get(&name).into_iter().flatten() {
            let (relative, fragment) = reference.split_once('#').unwrap_or((reference, ""));
            let uri = resolve_uri(base, relative)?;
            let fragment = decode_fragment(fragment)?;
            let (target_name, resource_path, pointer) =
                if fragment.is_empty() || fragment.starts_with('/') {
                    let (target_name, path) = resources.get(&uri).ok_or_else(|| {
                        bundle_error(format!("unresolved schema document {uri:?}"))
                    })?;
                    (target_name, path, fragment.as_str())
                } else {
                    let (target_name, path) =
                        anchors.get(&format!("{uri}#{fragment}")).ok_or_else(|| {
                            bundle_error(format!("unresolved schema anchor {fragment:?}"))
                        })?;
                    (target_name, path, "")
                };
            documents
                .get(target_name)
                .and_then(|document| document.pointer(resource_path))
                .and_then(|resource| resource.pointer(pointer))
                .filter(|target| target.is_object() || target.is_boolean())
                .ok_or_else(|| {
                    bundle_error(format!("unresolved schema reference {reference:?}"))
                })?;
            pending.push_back(target_name.clone());
        }
    }
    Ok(used)
}

fn insert_location(
    index: &mut BTreeMap<String, ResourceLocation>,
    uri: String,
    location: ResourceLocation,
) -> Result<(), SchemaBundleError> {
    if index.get(&uri).is_some_and(|old| old != &location) {
        return Err(bundle_error(format!("ambiguous schema URI {uri:?}")));
    }
    index.insert(uri, location);
    Ok(())
}

fn walk_schema(
    schema: &Value,
    base: &str,
    path: &str,
    depth: usize,
    visit: &mut impl FnMut(&Value, &str, &str) -> Result<(), SchemaBundleError>,
) -> Result<(), SchemaBundleError> {
    if depth > 64 || !(schema.is_object() || schema.is_boolean()) {
        return Err(bundle_error(
            "schema node must be an object or boolean within depth 64",
        ));
    }
    let base = match schema.get("$id") {
        Some(value) => resolve_uri(
            base,
            value
                .as_str()
                .filter(|id| !id.is_empty() && !id.contains('#'))
                .ok_or_else(|| bundle_error("invalid schema $id"))?,
        )?,
        None => base.to_owned(),
    };
    if schema
        .get("$schema")
        .is_some_and(|uri| uri.as_str() != Some("https://json-schema.org/draft/2020-12/schema"))
    {
        return Err(bundle_error("opaque schemas must use JSON Schema 2020-12"));
    }
    visit(schema, &base, path)?;
    for keyword in [
        "$defs",
        "definitions",
        "properties",
        "patternProperties",
        "dependentSchemas",
    ] {
        if let Some(children) = schema.get(keyword) {
            let children = children
                .as_object()
                .ok_or_else(|| bundle_error(format!("{keyword} must be an object")))?;
            for (name, child) in children {
                walk_schema(
                    child,
                    &base,
                    &format!("{path}/{keyword}/{}", pointer_token(name)),
                    depth + 1,
                    visit,
                )?;
            }
        }
    }
    for keyword in ["allOf", "anyOf", "oneOf", "prefixItems"] {
        if let Some(children) = schema.get(keyword) {
            let children = children
                .as_array()
                .ok_or_else(|| bundle_error(format!("{keyword} must be an array")))?;
            for (index, child) in children.iter().enumerate() {
                walk_schema(
                    child,
                    &base,
                    &format!("{path}/{keyword}/{index}"),
                    depth + 1,
                    visit,
                )?;
            }
        }
    }
    for keyword in [
        "items",
        "additionalProperties",
        "additionalItems",
        "unevaluatedProperties",
        "unevaluatedItems",
        "contains",
        "propertyNames",
        "not",
        "if",
        "then",
        "else",
        "contentSchema",
    ] {
        if let Some(child) = schema.get(keyword) {
            walk_schema(child, &base, &format!("{path}/{keyword}"), depth + 1, visit)?;
        }
    }
    Ok(())
}

fn resolve_uri(base: &str, relative: &str) -> Result<String, SchemaBundleError> {
    if relative.contains('#')
        || relative
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
    {
        return Err(bundle_error("invalid schema URI"));
    }
    if relative.is_empty() {
        return Ok(base.to_owned());
    }
    if relative
        .split('/')
        .next()
        .is_some_and(|part| part.contains(':'))
    {
        return Ok(relative.to_owned());
    }
    if relative.starts_with("//") {
        let scheme = base
            .split_once(':')
            .map(|(scheme, _)| scheme)
            .ok_or_else(|| bundle_error("relative authority requires a URI scheme"))?;
        return Ok(format!("{scheme}:{relative}"));
    }
    let base = base.split('?').next().unwrap_or(base);
    let combined = if relative.starts_with('/') {
        if let Some((scheme, rest)) = base.split_once("://") {
            let authority = rest.split('/').next().unwrap_or(rest);
            format!("{scheme}://{authority}{relative}")
        } else {
            relative.to_owned()
        }
    } else {
        format!(
            "{}{relative}",
            base.rsplit_once('/')
                .map(|(directory, _)| format!("{directory}/"))
                .unwrap_or_default()
        )
    };
    let (prefix, path) = if let Some((scheme, rest)) = combined.split_once("://") {
        match rest.split_once('/') {
            Some((authority, path)) => (format!("{scheme}://{authority}/"), path),
            None => return Ok(combined),
        }
    } else if let Some(path) = combined.strip_prefix('/') {
        ("/".to_owned(), path)
    } else {
        (String::new(), combined.as_str())
    };
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    Ok(format!("{prefix}{}", parts.join("/")))
}

fn decode_fragment(fragment: &str) -> Result<String, SchemaBundleError> {
    let bytes = fragment.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let pair = bytes
                .get(index + 1..index + 3)
                .ok_or_else(|| bundle_error("invalid URI fragment escape"))?;
            let text = std::str::from_utf8(pair)
                .map_err(|_| bundle_error("invalid URI fragment escape"))?;
            decoded.push(
                u8::from_str_radix(text, 16)
                    .map_err(|_| bundle_error("invalid URI fragment escape"))?,
            );
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).map_err(|_| bundle_error("invalid UTF-8 URI fragment"))
}

fn pointer_token(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

fn valid_anchor(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn bundle_error(message: impl Into<String>) -> SchemaBundleError {
    SchemaBundleError(message.into())
}
