use proc_macro::TokenStream;
use proc_macro2::Span;
use proc_macro_error::{abort, proc_macro_error};
use quote::{quote, ToTokens, format_ident};
use serde_json;
use std::str::FromStr;
use syn::{
    parse_macro_input, punctuated::Punctuated, token::Comma, FnArg, FnArg::Typed, Ident, ItemFn,
    Lit, Meta, Pat, PatType, ReturnType, Token, Type,
};

const TYPE: &str = "type";
const OBJECT: &str = "object";
const DESCRIPTION: &str = "description";
const PROPERTIES: &str = "properties";
const REQUIRED: &str = "required";
const RETURNS: &str = "returns";
const RETRY_COUNT: &str = "retry_count";
const TIMEOUT: &str = "timeout";

fn map_type_to_schema(ty: &Type) -> serde_json::Value {
    match ty {
        Type::Path(tp) => {
            let segments = &tp.path.segments;
            let last_segment = segments.last().unwrap();
            let type_name = last_segment.ident.to_string();
            match type_name.as_str() {
                "String" => serde_json::json!({ "type": "string" }),
                "i32" | "u32" | "i64" | "u64" => serde_json::json!({ "type": "integer" }),
                "f32" | "f64" => serde_json::json!({ "type": "number" }),
                "bool" => serde_json::json!({ "type": "boolean" }),
                "Vec" => {
                    if let syn::PathArguments::AngleBracketed(args) = &last_segment.arguments {
                        if let Some(arg) = args.args.first() {
                            if let syn::GenericArgument::Type(inner_type) = arg {
                                let inner_schema = map_type_to_schema(inner_type);
                                return serde_json::json!({
                                    "type": "array",
                                    "items": inner_schema
                                });
                            }
                        }
                    }
                    serde_json::json!({ "type": "array" })
                }
                "Option" => {
                    if let syn::PathArguments::AngleBracketed(args) = &last_segment.arguments {
                        if let Some(arg) = args.args.first() {
                            if let syn::GenericArgument::Type(inner_type) = arg {
                                return map_type_to_schema(inner_type);
                            }
                        }
                    }
                    serde_json::json!({ "type": "null" })
                }
                "TaskOutput" => serde_json::json!({
                    "type": "object",
                    "properties": {
                        "success": {"type": "boolean"},
                        "data": {"type": ["object", "null"], "additionalProperties": true},
                        "error": {"type": ["string", "null"]}
                    },
                    "required": ["success"]
                }),
                _ => serde_json::json!({ "type": "object", "additionalProperties": false }),
            }
        }
        _ => serde_json::json!({ "type": "object", "additionalProperties": false }),
    }
}

#[proc_macro_attribute]
#[proc_macro_error]
pub fn action(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attr_args =
        syn::parse::Parser::parse2(Punctuated::<Meta, Token![,]>::parse_terminated, attr.into())
            .unwrap_or_else(|e| abort!(e.span(), "Failed to parse action attributes: {}", e));

    // Parse attributes
    let mut description = "No description provided".to_string();
    let mut retry_count: Option<u32> = None;
    let mut timeout: Option<u64> = None; // Timeout in seconds

    for meta in &attr_args {
        if let Meta::NameValue(nv) = meta {
            if nv.path.is_ident(DESCRIPTION) {
                if let syn::Expr::Lit(expr_lit) = &nv.value {
                    if let Lit::Str(lit) = &expr_lit.lit {
                        description = lit.value();
                    } else {
                        abort!(expr_lit, "Expected a string literal for description");
                    }
                } else {
                    abort!(nv.value, "Expected a string literal for description");
                }
            } else if nv.path.is_ident(RETRY_COUNT) {
                if let syn::Expr::Lit(expr_lit) = &nv.value {
                    if let Lit::Int(lit) = &expr_lit.lit {
                        retry_count = Some(lit.base10_parse::<u32>().unwrap_or_else(|_| {
                            abort!(expr_lit, "Expected an integer for retry_count")
                        }));
                    } else {
                        abort!(expr_lit, "Expected an integer for retry_count");
                    }
                } else {
                    abort!(nv.value, "Expected an integer for retry_count");
                }
            } else if nv.path.is_ident(TIMEOUT) {
                if let syn::Expr::Lit(expr_lit) = &nv.value {
                    if let Lit::Int(lit) = &expr_lit.lit {
                        timeout = Some(lit.base10_parse::<u64>().unwrap_or_else(|_| {
                            abort!(expr_lit, "Expected an integer for timeout")
                        }));
                    } else {
                        abort!(expr_lit, "Expected an integer for timeout");
                    }
                } else {
                    abort!(nv.value, "Expected an integer for timeout");
                }
            }
        }
    }

    let input = parse_macro_input!(item as ItemFn);
    let fn_name = &input.sig.ident;
    let fn_name_str = fn_name.to_string();
    let fn_vis = &input.vis;
    let struct_name = syn::Ident::new(&format!("__{}Action", fn_name_str), fn_name.span());
    let static_name = syn::Ident::new(&fn_name_str.to_uppercase(), fn_name.span());

    // Parse all inputs, including additional parameters
    let inputs: Vec<(String, Type, Option<String>, bool)> = input.sig.inputs.iter().filter_map(|arg| {
        if let Typed(PatType { pat, ty, attrs, .. }) = arg {
            let param_name = match pat.as_ref() {
                Pat::Ident(pat_ident) => pat_ident.ident.to_string(),
                _ => return None,
            };
            let is_optional = matches!(&**ty, Type::Path(tp) if tp.path.segments.iter().any(|seg| seg.ident == "Option"));
            let description = attrs.iter().find_map(|attr| {
                if attr.path().is_ident("param") {
                    if let Ok(syn::Meta::NameValue(nv)) = attr.parse_args() {
                        if nv.path.is_ident(DESCRIPTION) {
                            if let syn::Expr::Lit(expr_lit) = &nv.value {
                                if let syn::Lit::Str(lit) = &expr_lit.lit {
                                    return Some(lit.value());
                                } else {
                                    abort!(expr_lit, "Expected a string literal for param description");
                                }
                            } else {
                                abort!(nv.value, "Expected a string literal for param description");
                            }
                        } else {
                            abort!(nv.path, "Only 'description' is supported in #[param]");
                        }
                    } else {
                        abort!(attr, "Expected key-value #[param] attributes");
                    }
                }
                None
            });
            Some((param_name, *ty.clone(), description, is_optional))
        } else {
            None
        }
    }).collect();

    // Parse return type
    let return_type = match &input.sig.output {
        ReturnType::Default => syn::parse_quote!(()),
        ReturnType::Type(_, ty) => (*ty).clone(),
    };
    let return_type_schema = map_type_to_schema(&return_type);

    // Check for standard DAG executor parameters
    let has_executor_params = inputs.len() >= 3
        && inputs[0].0 == "_executor"
        && inputs[1].0 == "node"
        && inputs[2].0 == "cache";
    let extra_params = if has_executor_params {
        &inputs[3..] // Skip _executor, node, cache
    } else {
        &inputs[..] // All params are extra if no standard params
    };

    // Build properties for extra parameters
    let properties: serde_json::Map<String, serde_json::Value> = extra_params
        .iter()
        .map(|(name, ty, desc, is_optional)| {
            let type_schema = map_type_to_schema(ty);
            let schema = if *is_optional {
                serde_json::json!({
                    "description": desc.as_ref().unwrap_or(&String::new()),
                    "type": ["null", type_schema["type"]]
                })
            } else {
                serde_json::json!({
                    "description": desc.as_ref().unwrap_or(&String::new()),
                    "type": type_schema["type"]
                })
            };
            (name.clone(), schema)
        })
        .collect();

    let required: Vec<String> = extra_params
        .iter()
        .filter(|(_, _, _, is_opt)| !is_opt)
        .map(|(name, _, _, _)| name.clone())
        .collect();

    let params_schema = if extra_params.len() == 1 && !extra_params[0].3 {
        map_type_to_schema(&extra_params[0].1)
    } else {
        serde_json::json!({
            TYPE: OBJECT,
            PROPERTIES: properties,
            REQUIRED: required,
            "additionalProperties": false
        })
    };

    let full_schema = serde_json::json!({
        "name": fn_name_str,
        DESCRIPTION: description,
        "parameters": params_schema,
        RETURNS: return_type_schema,
        "additionalProperties": false
    });

    let schema_string = serde_json::to_string(&full_schema).unwrap_or_else(|err| {
        abort!(
            input.sig.ident,
            "Failed to serialize schema to JSON: {}",
            err
        )
    });

    // Generate argument list for calling the function
    let arg_names: Vec<Ident> = inputs
        .iter()
        .map(|(name, _, _, _)| Ident::new(name, fn_name.span()))
        .collect();
    let execute_call = if has_executor_params {
        let extra_args = &arg_names[3..];
        quote! {
            #fn_name(executor, node, cache, #(#extra_args),*).await
        }
    } else {
        quote! {
            #fn_name(#(#arg_names),*).await
        }
    };

    // Default values if not provided
    let default_retry_count = retry_count.unwrap_or(1);
    let default_timeout = timeout.unwrap_or(30);

    let expanded = quote! {
        #input

        #[allow(non_camel_case_types)]
        #fn_vis struct #struct_name;

        impl Clone for #struct_name {
            fn clone(&self) -> Self { #struct_name {} }
        }

        #[::async_trait::async_trait]
        impl ::dagger::NodeAction for #struct_name {
            fn name(&self) -> String { #fn_name_str.to_string() }
            async fn execute(&self, executor: &mut ::dagger::DagExecutor, node: &::dagger::Node, cache: &::dagger::Cache) -> ::anyhow::Result<()> {
                use ::tokio::time::{timeout, Duration};

                let mut result = Err(::anyhow::anyhow!("Function did not execute successfully"));

                for attempt in 0..#default_retry_count {
                    // Define the future explicitly before passing it to timeout
                    let future = async {
                        #execute_call
                    };

                    match timeout(Duration::from_secs(#default_timeout), future).await {
                        Ok(Ok(res)) => {
                            result = Ok(res);
                            break; // Success, exit the retry loop
                        },
                        Ok(Err(e)) => {
                            // Function execution error
                            result = Err(e);
                            // Optionally log or handle the error
                            eprintln!("Attempt {} failed: {:?}", attempt + 1, result.as_ref().err());
                        },
                        Err(_) => {
                            // Timeout occurred
                            result = Err(::anyhow::anyhow!("Timeout after {} seconds", #default_timeout));
                            eprintln!("Attempt {} timed out", attempt + 1);
                        }
                    }

                    if attempt < #default_retry_count -1 {
                        //To avoid the last sleep
                       ::tokio::time::sleep(::tokio::time::Duration::from_millis(500)).await;
                    }
                }

                let final_result = result?;
                ::dagger::insert_value(cache, &node.id, "result", &final_result)?;
                Ok(())
            }
            fn schema(&self) -> ::serde_json::Value {
                ::serde_json::from_str(#schema_string).expect("Invalid JSON generated internally")
            }
        }

        #fn_vis static #static_name: #struct_name = #struct_name {};
    };

    TokenStream::from(expanded)
}

/// Proc macro to define a PubSubAgent from a function.
///
/// # Attributes
/// - `name`: A string identifier for the agent (required).
/// - `description`: A string describing the agent.
/// - `subscribe`: A comma-separated list of subscription channels (e.g., `"tasks, updates"`).
/// - `publish`: A comma-separated list of publication channels (e.g., `"results"`).
/// - `input_schema`: A JSON string defining the input schema.
/// - `output_schema`: A JSON string defining the output schema.
///
/// # Example
/// ```rust
/// #[pubsub_agent(
///     name = "TaskProcessor",
///     description = "Processes tasks and publishes results",
///     subscribe = "tasks",
///     publish = "results",
///     input_schema = r#"{"type": "object", "properties": {"task": {"type": "string"}}}"#,
///     output_schema = r#"{"type": "object", "properties": {"result": {"type": "string"}}}"#
/// )]
/// async fn task_processor(
///     node_id: &str,
///     channel: &str,
///     message: Message,
///     executor: &mut PubSubExecutor,
///     cache: &Cache
/// ) -> Result<()> {
///     let task = message.payload["task"].as_str().ok_or(anyhow!("Missing task"))?;
///     let result_msg = Message::new(node_id.to_string(), json!({"result": format!("Processed: {}", task)}));
///     executor.publish("results", result_msg, cache).await?;
///     Ok(())
/// }
/// ```

#[proc_macro_attribute]
#[proc_macro_error]
pub fn pubsub_agent(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attr_args =
        syn::parse::Parser::parse2(Punctuated::<Meta, Token![,]>::parse_terminated, attr.into())
            .unwrap_or_else(|e| abort!(e.span(), "Failed to parse pubsub_agent attributes: {}", e));

    // Required: name
    let name = attr_args
        .iter()
        .find_map(|meta| {
            if let Meta::NameValue(nv) = meta {
                if nv.path.is_ident("name") {
                    if let syn::Expr::Lit(expr_lit) = &nv.value {
                        if let Lit::Str(lit) = &expr_lit.lit {
                            return Some(lit.value());
                        } else {
                            abort!(expr_lit, "Expected a string literal for name");
                        }
                    } else {
                        abort!(nv.value, "Expected a string literal for name");
                    }
                }
            }
            None
        })
        .unwrap_or_else(|| {
            abort!(
                attr_args,
                "Missing required 'name' attribute for pubsub_agent"
            )
        });

    // Optional: description
    let description = attr_args
        .iter()
        .find_map(|meta| {
            if let Meta::NameValue(nv) = meta {
                if nv.path.is_ident("description") {
                    if let syn::Expr::Lit(expr_lit) = &nv.value {
                        if let Lit::Str(lit) = &expr_lit.lit {
                            return Some(lit.value());
                        } else {
                            abort!(expr_lit, "Expected a string literal for description");
                        }
                    } else {
                        abort!(nv.value, "Expected a string literal for description");
                    }
                }
            }
            None
        })
        .unwrap_or("No description provided".to_string());

    // Required: subscribe
    let subscribe = attr_args
        .iter()
        .find_map(|meta| {
            if let Meta::NameValue(nv) = meta {
                if nv.path.is_ident("subscribe") {
                    if let syn::Expr::Lit(expr_lit) = &nv.value {
                        if let Lit::Str(lit) = &expr_lit.lit {
                            return Some(
                                lit.value()
                                    .split(',')
                                    .map(|s| s.trim().to_string())
                                    .collect::<Vec<_>>(),
                            );
                        } else {
                            abort!(expr_lit, "Expected a string literal for subscribe");
                        }
                    } else {
                        abort!(nv.value, "Expected a string literal for subscribe");
                    }
                }
            }
            None
        })
        .unwrap_or_else(|| vec![]);

    // Optional: publish
    let publish = attr_args
        .iter()
        .find_map(|meta| {
            if let Meta::NameValue(nv) = meta {
                if nv.path.is_ident("publish") {
                    if let syn::Expr::Lit(expr_lit) = &nv.value {
                        if let Lit::Str(lit) = &expr_lit.lit {
                            return Some(
                                lit.value()
                                    .split(',')
                                    .map(|s| s.trim().to_string())
                                    .collect::<Vec<_>>(),
                            );
                        } else {
                            abort!(expr_lit, "Expected a string literal for publish");
                        }
                    } else {
                        abort!(nv.value, "Expected a string literal for publish");
                    }
                }
            }
            None
        })
        .unwrap_or_else(|| vec![]);

    // Optional: input_schema and output_schema
    let input_schema_str = attr_args
        .iter()
        .find_map(|meta| {
            if let Meta::NameValue(nv) = meta {
                if nv.path.is_ident("input_schema") {
                    if let syn::Expr::Lit(expr_lit) = &nv.value {
                        if let Lit::Str(lit) = &expr_lit.lit {
                            return Some(lit.value());
                        } else {
                            abort!(expr_lit, "Expected a string literal for input_schema");
                        }
                    } else {
                        abort!(nv.value, "Expected a string literal for input_schema");
                    }
                }
            }
            None
        })
        .unwrap_or(r#"{"type": "object", "additionalProperties": true}"#.to_string());

    let output_schema_str = attr_args
        .iter()
        .find_map(|meta| {
            if let Meta::NameValue(nv) = meta {
                if nv.path.is_ident("output_schema") {
                    if let syn::Expr::Lit(expr_lit) = &nv.value {
                        if let Lit::Str(lit) = &expr_lit.lit {
                            return Some(lit.value());
                        } else {
                            abort!(expr_lit, "Expected a string literal for output_schema");
                        }
                    } else {
                        abort!(nv.value, "Expected a string literal for output_schema");
                    }
                }
            }
            None
        })
        .unwrap_or(r#"{"type": "object", "additionalProperties": true}"#.to_string());

    let input = parse_macro_input!(item as ItemFn);
    let fn_name = &input.sig.ident;
    let fn_vis = &input.vis;
    let struct_name = syn::Ident::new(&format!("__{}Agent", fn_name.to_string()), fn_name.span());

    // Validate schemas at compile time
    if let Err(e) = serde_json::from_str::<serde_json::Value>(&input_schema_str) {
        abort!(input.sig, "Invalid input_schema JSON: {}", e);
    }
    if let Err(e) = serde_json::from_str::<serde_json::Value>(&output_schema_str) {
        abort!(input.sig, "Invalid output_schema JSON: {}", e);
    }

    // Parse function inputs dynamically
    let inputs: Vec<(String, Box<Type>)> = input
        .sig
        .inputs
        .iter()
        .filter_map(|arg| {
            if let Typed(PatType { pat, ty, .. }) = arg {
                let param_name = match pat.as_ref() {
                    Pat::Ident(pat_ident) => pat_ident.ident.to_string(),
                    _ => return None,
                };
                Some((param_name, ty.clone()))
            } else {
                None
            }
        })
        .collect();

    // Check required parameters
    let has_node_id = inputs.iter().any(|(name, _)| name == "node_id");
    let has_channel = inputs.iter().any(|(name, _)| name == "channel");
    let has_message = inputs.iter().any(|(name, _)| name == "message");
    let has_executor = inputs.iter().any(|(name, _)| name == "executor");
    let has_cache = inputs.iter().any(|(name, _)| name == "cache");

    if !(has_node_id && has_channel && has_message && has_executor && has_cache) {
        abort!(input.sig.inputs, "Function must include `node_id: &str`, `channel: &str`, `message: Message`, `executor: &mut PubSubExecutor`, and `cache: &Cache` parameters");
    }

    // Generate call with all parameters
    let fn_args: Vec<proc_macro2::TokenStream> = inputs
        .iter()
        .map(|(name, _)| {
            match name.as_str() {
                "node_id" => quote! { node_id },
                "channel" => quote! { channel },
                "message" => quote! { &message },
                "executor" => quote! { executor },
                "cache" => quote! { &*cache }, // Dereference Arc<Cache> to &Cache
                _ => abort!(input.sig.inputs, "Unsupported parameter: {}", name),
            }
        })
        .collect();
    let fn_call = quote! { #fn_name(#(#fn_args),*).await };

    let subscriptions = subscribe.iter().map(|s| quote!(#s.to_string()));
    let publications = publish.iter().map(|s| quote!(#s.to_string()));
    let expanded = quote! {
        #input

        #[allow(non_camel_case_types)]
        #fn_vis struct #struct_name {
            input_schema: ::jsonschema::JSONSchema,
            input_schema_value: ::serde_json::Value,
            output_schema: ::jsonschema::JSONSchema,
            output_schema_value: ::serde_json::Value,
        }

        impl #struct_name {
            pub fn new() -> Self {
                let input_schema_value: ::serde_json::Value = ::serde_json::from_str(#input_schema_str)
                    .unwrap_or_else(|e| panic!("Failed to parse input schema: {}", e));
                let output_schema_value: ::serde_json::Value = ::serde_json::from_str(#output_schema_str)
                    .unwrap_or_else(|e| panic!("Failed to parse output schema: {}", e));
                Self {
                    input_schema: ::jsonschema::JSONSchema::compile(&input_schema_value)
                        .unwrap_or_else(|e| panic!("Failed to compile input schema: {}", e)),
                    input_schema_value,
                    output_schema: ::jsonschema::JSONSchema::compile(&output_schema_value)
                        .unwrap_or_else(|e| panic!("Failed to compile output schema: {}", e)),
                    output_schema_value,
                }
            }

            pub async fn publish_message(
                &self,
                node_id: &str,
                channel: &str,
                task_id: Option<String>,
                task_type: Option<&str>,
                payload: ::serde_json::Value,
                executor: &mut ::dagger::PubSubExecutor,
                cache: &::std::sync::Arc<::dagger::Cache>,
            ) -> ::anyhow::Result<String> {
                let source_id = executor.get_current_agent_id().unwrap_or_else(|| node_id.to_string());
                let mut msg = match task_id {
                    Some(id) => ::dagger::Message::with_task_id(source_id, id.clone(), payload),
                    None => ::dagger::Message::new(source_id, payload),
                };
                let task = task_type.map(|tt| (tt.to_string(), msg.payload.clone()));
                executor.publish(channel, msg).await.map_err(|e| ::anyhow::anyhow!(e))
            }
        }

        #[::async_trait::async_trait]
        impl ::dagger::PubSubAgent for #struct_name {
            fn name(&self) -> String { #name.to_string() }
            fn description(&self) -> String { #description.to_string() }
            fn subscriptions(&self) -> Vec<String> { vec![#(#subscriptions),*] }
            fn publications(&self) -> Vec<String> { vec![#(#publications),*] }
            fn input_schema(&self) -> ::serde_json::Value { self.input_schema_value.clone() }
            fn output_schema(&self) -> ::serde_json::Value { self.output_schema_value.clone() }

            async fn process_message(
                &self,
                node_id: &str,
                channel: &str,
                message: &::dagger::Message,
                executor: &mut ::dagger::PubSubExecutor,
                cache: ::std::sync::Arc<::dagger::Cache>,
            ) -> ::anyhow::Result<()> {
                let refit = format!("{}_{}", self.name(), ::chrono::Utc::now().timestamp_millis());
                let agent_idx = {
                    let mut tree = executor.execution_tree.write().await;
                    let agent_node = ::dagger::NodeSnapshot {
                        node_id: refit.clone(),
                        outcome: ::dagger::NodeExecutionOutcome {
                            node_id: self.name(),
                            success: true,
                            retry_messages: Vec::new(),
                            final_error: None,
                        },
                        cache_ref: refit.clone(),
                        timestamp: ::chrono::Local::now().naive_local(),
                        channel: Some(channel.to_string()),
                        message_id: message.task_id.clone(),
                    };
                    let agent_idx = tree.add_node(agent_node);
                    if let Some(task_id) = &message.task_id {
                        if let Some(source_idx) = tree.node_indices().find(|idx|
                            tree.node_weight(*idx).map_or(false, |node| node.message_id.as_ref() == Some(task_id))
                        ) {
                            tree.add_edge(source_idx, agent_idx, ::dagger::ExecutionEdge {
                                parent: task_id.clone(),
                                label: "processed_message".to_string(),
                            });
                        }
                    }
                    agent_idx
                };

                // if let Some(task_id) = &message.task_id {
                //     executor.task_manager.update_task_status(task_id, ::dagger::TaskStatus::InProgress).await?;
                // }

                self.validate_input(&message.payload)?;

                executor.set_current_agent_id(node_id.to_string());

                let message_clone = message.clone();

                let result = #fn_call;

                // if let Some(task_id) = &message.task_id {
                //     let status = if result.is_ok() { ::dagger::TaskStatus::Completed } else { ::dagger::TaskStatus::Failed };
                //     executor.task_manager.update_task_status(task_id, status).await?;
                // }

                executor.clear_current_agent_id();

                let mut tree = executor.execution_tree.write().await;
                match result {
                    Ok(()) => Ok(()),
                    Err(e) => {
                        if let Some(node) = tree.node_weight_mut(agent_idx) {
                            node.outcome.success = false;
                            node.outcome.final_error = Some(e.to_string());
                        }
                        Err(e)
                    }
                }
            }

            fn validate_input(&self, payload: &::serde_json::Value) -> ::anyhow::Result<()> {
                if let Err(errors) = self.input_schema.validate(payload) {
                    let error_messages: Vec<String> = errors.collect::<Vec<_>>().iter().map(|e| e.to_string()).collect();
                    Err(::anyhow::anyhow!("Input validation failed: {}", error_messages.join(", ")))
                } else {
                    Ok(())
                }
            }

            fn validate_output(&self, payload: &::serde_json::Value) -> ::anyhow::Result<()> {
                if let Err(errors) = self.output_schema.validate(payload) {
                    let error_messages: Vec<String> = errors.collect::<Vec<_>>().iter().map(|e| e.to_string()).collect();
                    Err(::anyhow::anyhow!("Output validation failed: {}", error_messages.join(", ")))
                } else {
                    Ok(())
                }
            }
        }

        // Create a module to contain the agent
        pub mod #fn_name {
            use super::*;
            
            // Create the agent struct
            #[allow(non_camel_case_types)]
            #fn_vis struct #struct_name;
            
            impl #struct_name {
                pub fn new() -> Self {
                    Self {}
                }
            }
            
            // Implement the TaskAgent trait
            #[::async_trait::async_trait]
            impl ::dagger::taskagent::TaskAgent for #struct_name {
                fn name(&self) -> String {
                    #name.to_string()
                }

                fn description(&self) -> String {
                    #description.to_string()
                }
                
                fn input_schema(&self) -> serde_json::Value {
                    serde_json::from_str(#input_schema_str).unwrap()
                }

                fn output_schema(&self) -> serde_json::Value {
                    serde_json::from_str(#output_schema_str).unwrap()
                }

                async fn execute(
                    &self,
                    task_id: &str,
                    input: serde_json::Value,
                    task_manager: &::dagger::taskagent::TaskManager,
                ) -> anyhow::Result<::dagger::taskagent::TaskOutput> {
                    use ::anyhow::anyhow;
                    
                    // Log task pickup
                    tracing::info!("[{}] Task picked up: {}", #name, task_id);
                    
                    // Validate input against schema
                    tracing::info!("[{}] Validating input for task: {}", #name, task_id);
                    match self.validate_input(&input) {
                        Ok(_) => tracing::info!("[{}] Input validation successful for task: {}", #name, task_id),
                        Err(e) => {
                            tracing::error!("[{}] Input validation failed for task {}: {}", #name, task_id, e);
                            return Err(e);
                        }
                    }
                    
                    // Get the task to extract the job_id
                    let task = match task_manager.get_task_by_id(task_id) {
                        Some(t) => t,
                        None => {
                            let err = anyhow!("Task not found: {}", task_id);
                            tracing::error!("[{}] {}", #name, err);
                            return Err(err);
                        }
                    };
                    
                    // Execute the function with the task manager
                    tracing::info!("[{}] Executing task function for task: {}", #name, task_id);
                    match super::#fn_name(input, task_id, &task.job_id, task_manager).await {
                        Ok(output) => {
                            tracing::info!("[{}] Task function execution successful for task: {}", #name, task_id);
                            
                            // Validate output against schema
                            tracing::info!("[{}] Validating output for task: {}", #name, task_id);
                            match self.validate_output(&output) {
                                Ok(_) => tracing::info!("[{}] Output validation successful for task: {}", #name, task_id),
                                Err(e) => {
                                    tracing::error!("[{}] Output validation failed for task {}: {}", #name, task_id, e);
                                    
                                    // Update task status to Failed with validation error
                                    if let Err(update_err) = task_manager.update_task_status(task_id, ::dagger::taskagent::TaskStatus::Failed) {
                                        tracing::error!("[{}] Failed to update task status to Failed: {}", #name, update_err);
                                    }
                                    
                                    return Err(e);
                                }
                            }
                            
                            // Store the output in the cache
                            tracing::info!("[{}] Storing output in cache for task: {}", #name, task_id);
                            if let Err(e) = task_manager.cache.insert_value(task_id, "output", &output) {
                                let err = anyhow!("Failed to store output in cache: {}", e);
                                tracing::error!("[{}] {}", #name, err);
                                return Err(err);
                            }
                            
                            // Update task status to Completed
                            let task_output = ::dagger::taskagent::TaskOutput {
                                success: true,
                                data: Some(output),
                                error: None,
                            };
                            
                            tracing::info!("[{}] Marking task as complete: {}", #name, task_id);
                            if let Err(e) = task_manager.complete_task(task_id, task_output.clone()) {
                                tracing::error!("[{}] Failed to update task status to Completed: {}", #name, e);
                            } else {
                                tracing::info!("[{}] Task successfully completed: {}", #name, task_id);
                            }
                            
                            Ok(task_output)
                        },
                        Err(err) => {
                            tracing::error!("[{}] Task function execution failed for task {}: {}", #name, task_id, err);
                            
                            // Update task status to Failed
                            tracing::info!("[{}] Marking task as failed: {}", #name, task_id);
                            if let Err(e) = task_manager.update_task_status(task_id, ::dagger::taskagent::TaskStatus::Failed) {
                                tracing::error!("[{}] Failed to update task status to Failed: {}", #name, e);
                            } else {
                                tracing::info!("[{}] Task marked as failed: {}", #name, task_id);
                            }
                            
                            Ok(::dagger::taskagent::TaskOutput {
                                success: false,
                                data: None,
                                error: Some(err),
                            })
                        },
                    }
                }
            }
            
            // Create a type alias using the function name
            pub type #fn_name = #struct_name;
            
            // Register the agent with the global registry
            #[::linkme::distributed_slice(::dagger::taskagent::TASK_AGENTS)]
            pub static AGENT_REGISTRATION: fn() -> Box<dyn ::dagger::taskagent::TaskAgent> = __register_agent;
            
            fn __register_agent() -> Box<dyn ::dagger::taskagent::TaskAgent> {
                Box::new(#struct_name::new())
            }
        }
    };

    TokenStream::from(expanded)
}

/// A macro to define a complete task workflow
#[proc_macro_attribute]
pub fn task_workflow(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as syn::ItemStruct);
    let attr_args =
        syn::parse::Parser::parse2(Punctuated::<Meta, Token![,]>::parse_terminated, attr.into())
            .unwrap_or_else(|e| abort!(e.span(), "Failed to parse task_workflow attributes: {}", e));
    
    let struct_name = &input.ident;
    
    // Parse attribute arguments
    let mut name = None;
    let mut description = None;
    
    for meta in &attr_args {
        if let Meta::NameValue(nv) = meta {
            if nv.path.is_ident("name") {
                if let syn::Expr::Lit(expr_lit) = &nv.value {
                    if let Lit::Str(lit) = &expr_lit.lit {
                        name = Some(lit.value());
                    } else {
                        abort!(expr_lit, "Expected a string literal for name");
                    }
                } else {
                    abort!(nv.value, "Expected a string literal for name");
                }
            } else if nv.path.is_ident("description") {
                if let syn::Expr::Lit(expr_lit) = &nv.value {
                    if let Lit::Str(lit) = &expr_lit.lit {
                        description = Some(lit.value());
                    } else {
                        abort!(expr_lit, "Expected a string literal for description");
                    }
                } else {
                    abort!(nv.value, "Expected a string literal for description");
                }
            }
        }
    }
    
    let workflow_name = name.unwrap_or_else(|| struct_name.to_string());
    let workflow_description = description.unwrap_or_else(|| format!("{} workflow", workflow_name));
    
    let expanded = quote! {
        #input
        
        impl #struct_name {
            /// Creates a new workflow instance
            pub fn new() -> Self {
                Self {
                    // Default initialization
                }
            }
            
            /// Gets the workflow name
            pub fn name(&self) -> &'static str {
                #workflow_name
            }
            
            /// Gets the workflow description
            pub fn description(&self) -> &'static str {
                #workflow_description
            }
            
            /// Creates a new task executor for this workflow
            pub fn create_executor(
                &self,
                task_manager: TaskManager,
                agent_registry: TaskAgentRegistry,
                cache: Cache,
                config: TaskConfiguration,
            ) -> TaskExecutor {
                let job_id = cuid2::create_id();
                TaskExecutor::new(
                    task_manager,
                    agent_registry,
                    cache,
                    config,
                    job_id,
                    None,
                )
            }
            
            /// Executes the workflow with the given initial tasks
            pub async fn execute(
                &self,
                mut executor: TaskExecutor,
                initial_tasks: Vec<Task>,
            ) -> anyhow::Result<TaskExecutionReport> {
                let job_id = executor.job_id.clone();
                let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
                
                // Store the cancel_tx somewhere if you need to cancel later
                
                executor.execute(job_id, initial_tasks, cancel_rx).await
            }
            
            /// Generates a DOT graph visualization of the workflow
            pub fn visualize(&self, executor: &TaskExecutor) -> String {
                executor.generate_detailed_dot_graph()
            }
        }
    };
    
    TokenStream::from(expanded)
}

#[proc_macro_attribute]
#[proc_macro_error]
pub fn task_agent(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input_fn = parse_macro_input!(item as ItemFn);
    let attr_args =
        syn::parse::Parser::parse2(Punctuated::<Meta, Token![,]>::parse_terminated, attr.into())
            .unwrap_or_else(|e| abort!(e.span(), "Failed to parse task_agent attributes: {}", e));

    // Extract required attributes
    let name = attr_args
        .iter()
        .find_map(|meta| {
            if let Meta::NameValue(nv) = meta {
                if nv.path.is_ident("name") {
                    if let syn::Expr::Lit(expr_lit) = &nv.value {
                        if let Lit::Str(lit) = &expr_lit.lit {
                            return Some(lit.value());
                        }
                    }
                }
            }
            None
        })
        .unwrap_or_else(|| abort!(proc_macro2::Span::call_site(), "Task agent requires a name attribute"));

    let description = attr_args
        .iter()
        .find_map(|meta| {
            if let Meta::NameValue(nv) = meta {
                if nv.path.is_ident("description") {
                    if let syn::Expr::Lit(expr_lit) = &nv.value {
                        if let Lit::Str(lit) = &expr_lit.lit {
                            return Some(lit.value());
                        }
                    }
                }
            }
            None
        })
        .unwrap_or_else(|| format!("Task agent for {}", name));

    let input_schema = attr_args
        .iter()
        .find_map(|meta| {
            if let Meta::NameValue(nv) = meta {
                if nv.path.is_ident("input_schema") {
                    if let syn::Expr::Lit(expr_lit) = &nv.value {
                        if let Lit::Str(lit) = &expr_lit.lit {
                            return Some(lit.value());
                        }
                    }
                }
            }
            None
        })
        .unwrap_or_else(|| abort!(proc_macro2::Span::call_site(), "Task agent requires an input_schema attribute"));

    let output_schema = attr_args
        .iter()
        .find_map(|meta| {
            if let Meta::NameValue(nv) = meta {
                if nv.path.is_ident("output_schema") {
                    if let syn::Expr::Lit(expr_lit) = &nv.value {
                        if let Lit::Str(lit) = &expr_lit.lit {
                            return Some(lit.value());
                        }
                    }
                }
            }
            None
        })
        .unwrap_or_else(|| abort!(proc_macro2::Span::call_site(), "Task agent requires an output_schema attribute"));

    // Validate schemas at compile time
    if let Err(e) = serde_json::from_str::<serde_json::Value>(&input_schema) {
        abort!(input_fn.sig, "Invalid input_schema JSON: {}", e);
    }
    if let Err(e) = serde_json::from_str::<serde_json::Value>(&output_schema) {
        abort!(input_fn.sig, "Invalid output_schema JSON: {}", e);
    }

    // Get function details
    let fn_name = &input_fn.sig.ident;
    let fn_vis = &input_fn.vis;
    let fn_block = &input_fn.block;
    
    // Generate unique identifiers
    let agent_struct_name = format_ident!("{}_Agent", fn_name);
    let module_name = fn_name;
    
    // Generate the complete implementation
    let expanded = quote! {
        // The original function is kept but made private
        #[allow(non_snake_case)]
        async fn #fn_name(
            input: serde_json::Value,
            task_id: &str,
            job_id: &str,
            task_manager: &::dagger::taskagent::TaskManager
        ) -> Result<serde_json::Value, String> {
            #fn_block
        }

        // Create a module to contain the agent
        pub mod #module_name {
            use super::*;
            
            // Create the agent struct
            #[allow(non_camel_case_types)]
            #fn_vis struct #agent_struct_name;
            
            impl #agent_struct_name {
                pub fn new() -> Self {
                    Self {}
                }
            }
            
            // Implement the TaskAgent trait
            #[::async_trait::async_trait]
            impl ::dagger::taskagent::TaskAgent for #agent_struct_name {
                fn name(&self) -> String {
                    #name.to_string()
                }

                fn description(&self) -> String {
                    #description.to_string()
                }
                
                fn input_schema(&self) -> serde_json::Value {
                    serde_json::from_str(#input_schema).unwrap()
                }

                fn output_schema(&self) -> serde_json::Value {
                    serde_json::from_str(#output_schema).unwrap()
                }

                async fn execute(
                    &self,
                    task_id: &str,
                    input: serde_json::Value,
                    task_manager: &::dagger::taskagent::TaskManager,
                ) -> anyhow::Result<::dagger::taskagent::TaskOutput> {
                    use ::anyhow::anyhow;
                    // task_manager.update_task_status(task_id, ::dagger::taskagent::TaskStatus::InProgress);
                    println!("🥵 Executing task: {}", task_id);
                    // Log task pickup
                    tracing::info!("[{}] Task picked up: {}", #name, task_id);
                    
                    // Validate input against schema
                    tracing::info!("[{}] Validating input for task: {}", #name, task_id);
                    match self.validate_input(&input) {
                        Ok(_) => tracing::info!("[{}] Input validation successful for task: {}", #name, task_id),
                        Err(e) => {
                            tracing::error!("[{}] Input validation failed for task {}: {}", #name, task_id, e);
                            return Err(e);
                        }
                    }
                    
                    // Get the task to extract the job_id
                    let task = match task_manager.get_task_by_id(task_id) {
                        Some(t) => t,
                        None => {
                            let err = anyhow!("Task not found: {}", task_id);
                            tracing::error!("[{}] {}", #name, err);
                            return Err(err);
                        }
                    };
                    
                    // Execute the function with the task manager
                    tracing::info!("[{}] Executing task function for task: {}", #name, task_id);
                    match super::#fn_name(input, task_id, &task.job_id, task_manager).await {
                        Ok(output) => {
                            println!("😱 Task function execution successful for task: {}", task_id);
                            tracing::info!("[{}] Task function execution successful for task: {}", #name, task_id);
                            
                            // Validate output against schema
                            tracing::info!("[{}] Validating output for task: {}", #name, task_id);
                            match self.validate_output(&output) {
                                Ok(_) => tracing::info!("[{}] Output validation successful for task: {}", #name, task_id),
                                Err(e) => {
                                    tracing::error!("[{}] Output validation failed for task {}: {}", #name, task_id, e);
                                    
                                    // Update task status to Failed with validation error
                                    if let Err(update_err) = task_manager.update_task_status(task_id, ::dagger::taskagent::TaskStatus::Failed) {
                                        tracing::error!("[{}] Failed to update task status to Failed: {}", #name, update_err);
                                    }
                                    
                                    return Err(e);
                                }
                            }
                            
                            // Store the output in the cache
                            tracing::info!("[{}] Storing output in cache for task: {}", #name, task_id);
                            if let Err(e) = task_manager.cache.insert_value(task_id, "output", &output) {
                                let err = anyhow!("Failed to store output in cache: {}", e);
                                tracing::error!("[{}] {}", #name, err);
                                return Err(err);
                            }
                            
                            // Update task status to Completed
                            let task_output = ::dagger::taskagent::TaskOutput {
                                success: true,
                                data: Some(output),
                                error: None,
                            };
                            
                            tracing::info!("[{}] Marking task as complete: {}", #name, task_id);
                            if let Err(e) = task_manager.complete_task(task_id, task_output.clone()) {
                                tracing::error!("[{}] Failed to update task status to Completed: {}", #name, e);
                            } else {
                                tracing::info!("[{}] Task successfully completed: {}", #name, task_id);
                            }
                            
                            Ok(task_output)
                        },
                        Err(err) => {
                                println!("🤢 Task function execution successful for task: {}", task_id);
                            tracing::error!("[{}] Task function execution failed for task {}: {}", #name, task_id, err);
                            
                            // Update task status to Failed
                            tracing::info!("[{}] Marking task as failed: {}", #name, task_id);
                            if let Err(e) = task_manager.update_task_status(task_id, ::dagger::taskagent::TaskStatus::Failed) {
                                tracing::error!("[{}] Failed to update task status to Failed: {}", #name, e);
                            } else {
                                tracing::info!("[{}] Task marked as failed: {}", #name, task_id);
                            }
                            
                            Ok(::dagger::taskagent::TaskOutput {
                                success: false,
                                data: None,
                                error: Some(err),
                            })
                        },
                    }
                }
            }
            
            // Create a type alias using the function name
            pub type #fn_name = #agent_struct_name;
            
            // Register the agent with the global registry
            #[::linkme::distributed_slice(::dagger::taskagent::TASK_AGENTS)]
            pub static AGENT_REGISTRATION: fn() -> Box<dyn ::dagger::taskagent::TaskAgent> = __register_agent;
            
            fn __register_agent() -> Box<dyn ::dagger::taskagent::TaskAgent> {
                Box::new(#agent_struct_name::new())
            }
        }
    };

    TokenStream::from(expanded)
}

#[proc_macro_attribute]
#[proc_macro_error]
pub fn task_builder(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as syn::ItemImpl);
    let attr_args =
        syn::parse::Parser::parse2(Punctuated::<Meta, Token![,]>::parse_terminated, attr.into())
            .unwrap_or_else(|e| abort!(e.span(), "Failed to parse task_builder attributes: {}", e));

    // Required: agent
    let agent = attr_args
        .iter()
        .find_map(|meta| {
            if let Meta::NameValue(nv) = meta {
                if nv.path.is_ident("agent") {
                    if let syn::Expr::Lit(expr_lit) = &nv.value {
                        if let Lit::Str(lit) = &expr_lit.lit {
                            return Some(lit.value());
                        } else {
                            abort!(expr_lit, "Expected a string literal for agent");
                        }
                    } else {
                        abort!(nv.value, "Expected a string literal for agent");
                    }
                }
            }
            None
        })
        .unwrap_or_else(|| abort!(proc_macro2::Span::call_site(), "Task builder requires an agent attribute"));

    let self_ty = &input.self_ty;

    // Generate the TaskBuilder struct and implementation
    let expanded = quote! {
        #input

        pub struct TaskBuilder {
            job_id: String,
            description: Option<String>,
            dependencies: Vec<String>,
            input: serde_json::Value,
            parent_task_id: Option<String>,
            acceptance_criteria: Option<String>,
            timeout: Option<u64>,
            max_retries: Option<u32>,
            agent_name: String,
        }

        impl #self_ty {
            pub fn create_task(&self, job_id: &str) -> TaskBuilder {
                TaskBuilder {
                    job_id: job_id.to_string(),
                    description: None,
                    dependencies: Vec::new(),
                    input: serde_json::json!({}),
                    parent_task_id: None,
                    acceptance_criteria: None,
                    timeout: None,
                    max_retries: None,
                    agent_name: #agent.to_string(),
                }
            }
        }

        impl TaskBuilder {
            pub fn with_input(mut self, input: serde_json::Value) -> Self {
                self.input = input;
                self
            }

            pub fn with_description(mut self, description: &str) -> Self {
                self.description = Some(description.to_string());
                self
            }

            pub fn add_dependency(mut self, task_id: &str) -> Self {
                self.dependencies.push(task_id.to_string());
                self
            }

            pub fn with_parent_task(mut self, parent_task_id: &str) -> Self {
                self.parent_task_id = Some(parent_task_id.to_string());
                self
            }

            pub fn with_acceptance_criteria(mut self, criteria: &str) -> Self {
                self.acceptance_criteria = Some(criteria.to_string());
                self
            }

            pub fn with_timeout(mut self, timeout_seconds: u64) -> Self {
                self.timeout = Some(timeout_seconds);
                self
            }

            pub fn with_max_retries(mut self, retries: u32) -> Self {
                self.max_retries = Some(retries);
                self
            }

            pub fn build(self, task_manager: &::dagger::taskagent::TaskManager) -> String {
                let task = ::dagger::taskagent::Task {
                    id: ::uuid::Uuid::new_v4().to_string(),
                    job_id: self.job_id,
                    agent: self.agent_name,
                    description: self.description.unwrap_or_else(|| "No description provided".to_string()),
                    dependencies: self.dependencies,
                    input: self.input,
                    status: ::dagger::taskagent::TaskStatus::Ready,
                    result: None,
                    parent_task_id: self.parent_task_id,
                    acceptance_criteria: self.acceptance_criteria,
                    timeout: self.timeout,
                    max_retries: self.max_retries,
                    retry_count: 0,
                    claimed_by: None,
                    created_at: ::chrono::Utc::now(),
                    updated_at: ::chrono::Utc::now(),
                };

                let task_id = task.id.clone();
                task_manager.add_task(task);
                task_id
            }
        }
    };

    TokenStream::from(expanded)
}