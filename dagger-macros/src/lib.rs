use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::{format_ident, quote};
use syn::{
    parse::Parser, parse_macro_input, FnArg, GenericArgument, ItemFn, Lit, Meta, MetaNameValue,
    PathArguments, ReturnType, Type,
};

use proc_macro_crate::{crate_name, FoundCrate};

fn dagger_path() -> syn::Path {
    match crate_name("dagger") {
        Ok(FoundCrate::Name(name)) => {
            let ident = syn::Ident::new(&name, Span::call_site());
            syn::parse_quote!(::#ident)
        }
        Ok(FoundCrate::Itself) => syn::parse_quote!(::dagger),
        Err(_) => syn::parse_quote!(::dagger),
    }
}

fn task_core_path() -> syn::Path {
    if let Ok(found) = crate_name("dagger") {
        let dagger: syn::Path = match found {
            FoundCrate::Name(name) => {
                let ident = syn::Ident::new(&name, Span::call_site());
                syn::parse_quote!(::#ident)
            }
            FoundCrate::Itself => syn::parse_quote!(::dagger),
        };
        syn::parse_quote!(#dagger::task_core)
    } else if let Ok(found) = crate_name("task-core") {
        match found {
            FoundCrate::Name(name) => {
                let ident = syn::Ident::new(&name, Span::call_site());
                syn::parse_quote!(::#ident)
            }
            FoundCrate::Itself => syn::parse_quote!(crate),
        }
    } else {
        syn::parse_quote!(::task_core)
    }
}

fn type_ends_with(ty: &Type, ident: &str) -> bool {
    match ty {
        Type::Path(path) => path
            .path
            .segments
            .last()
            .map(|seg| seg.ident == ident)
            .unwrap_or(false),
        Type::Reference(reference) => type_ends_with(&reference.elem, ident),
        _ => false,
    }
}

fn return_is_result(ret: &ReturnType) -> bool {
    match ret {
        ReturnType::Type(_, ty) => type_ends_with(ty, "Result"),
        _ => false,
    }
}

fn result_ok_type(ty: &Type) -> Option<&Type> {
    let Type::Path(path) = ty else {
        return None;
    };

    let segment = path.path.segments.last()?;
    if segment.ident != "Result" {
        return None;
    }

    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };

    args.args.iter().find_map(|arg| match arg {
        GenericArgument::Type(ty) => Some(ty),
        _ => None,
    })
}

fn return_ok_is_node_output(ret: &ReturnType) -> bool {
    match ret {
        ReturnType::Type(_, ty) => result_ok_type(ty)
            .map(|ok_ty| type_ends_with(ok_ty, "NodeOutput"))
            .unwrap_or_else(|| type_ends_with(ty, "NodeOutput")),
        _ => false,
    }
}

fn parse_string_attr(meta: &Meta, key: &str) -> Option<String> {
    match meta {
        Meta::NameValue(MetaNameValue { path, value, .. }) if path.is_ident(key) => {
            if let syn::Expr::Lit(expr_lit) = value {
                if let Lit::Str(lit) = &expr_lit.lit {
                    return Some(lit.value());
                }
            }
            None
        }
        _ => None,
    }
}

fn parse_string_list_attr(meta: &Meta, key: &str) -> Option<Vec<String>> {
    match meta {
        Meta::NameValue(MetaNameValue { path, value, .. }) if path.is_ident(key) => {
            match value {
                syn::Expr::Lit(expr_lit) => {
                    if let Lit::Str(lit) = &expr_lit.lit {
                        let items = lit
                            .value()
                            .split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect::<Vec<_>>();
                        return Some(items);
                    }
                }
                syn::Expr::Array(expr_array) => {
                    let mut items = Vec::new();
                    for elem in &expr_array.elems {
                        if let syn::Expr::Lit(expr_lit) = elem {
                            if let Lit::Str(lit) = &expr_lit.lit {
                                items.push(lit.value());
                            }
                        }
                    }
                    return Some(items);
                }
                _ => {}
            }
            None
        }
        _ => None,
    }
}

fn to_camel_case(name: &str) -> String {
    let mut out = String::new();
    for part in name.split('_') {
        if part.is_empty() {
            continue;
        }
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            out.push(first.to_ascii_uppercase());
            out.extend(chars.map(|c| c.to_ascii_lowercase()));
        }
    }
    out
}

/// Procedural macro for creating task agents
///
/// # Example
/// ```rust
/// use dagger_macros::task_agent;
/// use task_core::{Task, TaskContext};
///
/// #[task_agent(name = "calculator", description = "Performs calculations")]
/// async fn calculator(
///     _task: Task,
///     _ctx: std::sync::Arc<TaskContext>,
/// ) -> anyhow::Result<serde_json::Value> {
///     Ok(serde_json::json!({"ok": true}))
/// }
/// ```
#[proc_macro_attribute]
pub fn task_agent(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input_fn = parse_macro_input!(item as ItemFn);

    let parser = syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated;
    let attr_args = match parser.parse(attr.clone()) {
        Ok(args) => args,
        Err(e) => return TokenStream::from(syn::Error::to_compile_error(&e)),
    };

    let mut name = None;
    let mut description = None;

    for meta in attr_args {
        if let Some(value) = parse_string_attr(&meta, "name") {
            name = Some(value);
        } else if let Some(value) = parse_string_attr(&meta, "description") {
            description = Some(value);
        }
    }

    if input_fn.sig.asyncness.is_none() {
        return TokenStream::from(quote! {
            compile_error!("task_agent requires an async function");
        });
    }

    let arg_count = input_fn.sig.inputs.len();
    if arg_count == 0 || arg_count > 2 {
        return TokenStream::from(quote! {
            compile_error!("task_agent expects a function with 1 or 2 parameters");
        });
    }

    if !return_is_result(&input_fn.sig.output) {
        return TokenStream::from(quote! {
            compile_error!("task_agent expects the function to return Result<T, E>");
        });
    }

    let fn_name = &input_fn.sig.ident;
    let fn_vis = &input_fn.vis;
    let base_name = to_camel_case(&fn_name.to_string());
    let struct_name = format_ident!("{}Agent", base_name);

    let name = name.unwrap_or_else(|| fn_name.to_string());
    let description = description.unwrap_or_else(|| "".to_string());
    let agent_id = const_hash16(&name);

    let call = if arg_count == 1 {
        quote!(#fn_name(task).await)
    } else {
        quote!(#fn_name(task, ctx).await)
    };

    let task_core = task_core_path();
    let register_name = format_ident!("__{}_AGENT_REG", fn_name.to_string().to_ascii_uppercase());

    let output = quote! {
        #input_fn

        #[derive(Clone)]
        #fn_vis struct #struct_name;

        impl #struct_name {
            pub const AGENT_ID: u16 = #agent_id;
            pub const NAME: &'static str = #name;
            pub const DESCRIPTION: &'static str = #description;
        }

        #[#task_core::async_trait::async_trait]
        impl #task_core::Agent for #struct_name {
            async fn execute(
                &self,
                task: #task_core::Task,
                ctx: std::sync::Arc<#task_core::TaskContext>,
            ) -> std::result::Result<#task_core::Bytes, #task_core::AgentError> {
                let result = #call?;
                let output = #task_core::IntoBytes::into_bytes(result)?;
                Ok(output)
            }
        }

        #[#task_core::linkme::distributed_slice(#task_core::AGENTS)]
        static #register_name: fn(&mut #task_core::AgentRegistry) = |registry| {
            registry
                .register(
                    #struct_name::AGENT_ID,
                    #struct_name::NAME,
                    std::sync::Arc::new(#struct_name),
                )
                .expect("Failed to register agent");
        };
    };

    TokenStream::from(output)
}

/// Simple const hash function for agent IDs
const fn const_hash16(s: &str) -> u16 {
    let mut hash = 0u16;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        hash = hash.wrapping_mul(31).wrapping_add(bytes[i] as u16);
        i += 1;
    }
    hash
}

/// Macro for compute-only DAG node actions
#[proc_macro_attribute]
pub fn action(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input_fn = parse_macro_input!(item as ItemFn);

    let parser = syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated;
    let attr_args = match parser.parse(attr) {
        Ok(args) => args,
        Err(e) => return TokenStream::from(syn::Error::to_compile_error(&e)),
    };

    let mut name = None;
    let mut description = None;
    let mut input_schema = None;
    let mut output_schema = None;

    for meta in attr_args {
        if let Some(value) = parse_string_attr(&meta, "name") {
            name = Some(value);
        } else if let Some(value) = parse_string_attr(&meta, "description") {
            description = Some(value);
        } else if let Some(value) = parse_string_attr(&meta, "input_schema") {
            input_schema = Some(value);
        } else if let Some(value) = parse_string_attr(&meta, "output_schema") {
            output_schema = Some(value);
        }
    }

    if input_fn.sig.asyncness.is_none() {
        return TokenStream::from(quote! {
            compile_error!("action requires an async function");
        });
    }

    let arg_count = input_fn.sig.inputs.len();
    if arg_count != 1 {
        return TokenStream::from(quote! {
            compile_error!("action expects a function with exactly 1 parameter");
        });
    }

    if !return_is_result(&input_fn.sig.output) {
        return TokenStream::from(quote! {
            compile_error!("action expects the function to return Result<T, E>");
        });
    }

    let fn_name = &input_fn.sig.ident;
    let fn_vis = &input_fn.vis;
    let base_name = to_camel_case(&fn_name.to_string());
    let struct_name = format_ident!("{}Action", base_name);
    let action_name = name.unwrap_or_else(|| fn_name.to_string());
    let _description = description.unwrap_or_else(|| "".to_string());

    let param_ty = match input_fn.sig.inputs.first().unwrap() {
        FnArg::Typed(pat_type) => &*pat_type.ty,
        FnArg::Receiver(_) => {
            return TokenStream::from(quote! {
                compile_error!("action must be a free function, not a method");
            });
        }
    };

    let dagger = dagger_path();

    let call = match param_ty {
        Type::Reference(reference) if type_ends_with(&reference.elem, "NodeCtx") => {
            quote!(#fn_name(ctx).await)
        }
        Type::Path(_) if type_ends_with(param_ty, "NodeCtx") => {
            quote!(#fn_name(ctx.clone()).await)
        }
        _ => quote!({
            let input: #param_ty = #dagger::serde_json::from_value(ctx.inputs.clone())?;
            #fn_name(input).await
        }),
    };

    let wrap_output = if return_ok_is_node_output(&input_fn.sig.output) {
        quote!(result)
    } else {
        quote!({
            let value = #dagger::serde_json::to_value(result)?;
            #dagger::coord::action::NodeOutput::success(value)
        })
    };

    let input_schema_tokens = match input_schema {
        Some(schema) => quote!(Some(#schema)),
        None => quote!(None),
    };

    let output_schema_tokens = match output_schema {
        Some(schema) => quote!(Some(#schema)),
        None => quote!(None),
    };

    let register_name = format_ident!("__{}_ACTION_REG", fn_name.to_string().to_ascii_uppercase());

    let expanded = quote! {
        #input_fn

        #[derive(Debug, Clone)]
        #fn_vis struct #struct_name;

        #[#dagger::async_trait::async_trait]
        impl #dagger::coord::action::NodeAction for #struct_name {
            fn name(&self) -> &str {
                #action_name
            }

            async fn execute(
                &self,
                ctx: &#dagger::coord::action::NodeCtx,
            ) -> #dagger::anyhow::Result<#dagger::coord::action::NodeOutput> {
                let result = #call?;
                Ok(#wrap_output)
            }

            fn input_schema(&self) -> Option<&str> {
                #input_schema_tokens
            }

            fn output_schema(&self) -> Option<&str> {
                #output_schema_tokens
            }
        }

        #[#dagger::linkme::distributed_slice(#dagger::coord::registry::ACTION_REGISTRARS)]
        #fn_vis static #register_name: fn(&#dagger::coord::registry::ActionRegistry) = |registry| {
            registry.register(std::sync::Arc::new(#struct_name));
        };
    };

    TokenStream::from(expanded)
}

/// Macro for pub/sub agents
#[proc_macro_attribute]
pub fn pubsub_agent(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input_fn = parse_macro_input!(item as ItemFn);

    let parser = syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated;
    let attr_args = match parser.parse(attr) {
        Ok(args) => args,
        Err(e) => return TokenStream::from(syn::Error::to_compile_error(&e)),
    };

    let mut name = None;
    let mut description = None;
    let mut subscriptions = None;
    let mut publications = None;
    let mut input_schema = None;
    let mut output_schema = None;

    for meta in attr_args {
        if let Some(value) = parse_string_attr(&meta, "name") {
            name = Some(value);
        } else if let Some(value) = parse_string_attr(&meta, "description") {
            description = Some(value);
        } else if let Some(values) = parse_string_list_attr(&meta, "subscribe") {
            subscriptions = Some(values);
        } else if let Some(values) = parse_string_list_attr(&meta, "publish") {
            publications = Some(values);
        } else if let Some(value) = parse_string_attr(&meta, "input_schema") {
            input_schema = Some(value);
        } else if let Some(value) = parse_string_attr(&meta, "output_schema") {
            output_schema = Some(value);
        }
    }

    if input_fn.sig.asyncness.is_none() {
        return TokenStream::from(quote! {
            compile_error!("pubsub_agent requires an async function");
        });
    }

    let arg_count = input_fn.sig.inputs.len();
    if arg_count == 0 || arg_count > 2 {
        return TokenStream::from(quote! {
            compile_error!("pubsub_agent expects a function with 1 or 2 parameters");
        });
    }

    if !return_is_result(&input_fn.sig.output) {
        return TokenStream::from(quote! {
            compile_error!("pubsub_agent expects the function to return Result<T, E>");
        });
    }

    let fn_name = &input_fn.sig.ident;
    let fn_vis = &input_fn.vis;
    let base_name = to_camel_case(&fn_name.to_string());
    let struct_name = format_ident!("{}PubSubAgent", base_name);
    let agent_name = name.unwrap_or_else(|| fn_name.to_string());
    let agent_desc = description.unwrap_or_else(|| "".to_string());

    let subs = subscriptions.unwrap_or_default();
    let pubs = publications.unwrap_or_default();

    let subs_tokens = subs
        .iter()
        .map(|s| quote!(#s.to_string()))
        .collect::<Vec<_>>();
    let pubs_tokens = pubs
        .iter()
        .map(|s| quote!(#s.to_string()))
        .collect::<Vec<_>>();

    let dagger = dagger_path();

    let input_schema_tokens = match input_schema {
        Some(schema) => {
            quote!(#dagger::serde_json::from_str(#schema).unwrap_or_else(|_| #dagger::serde_json::json!({})))
        }
        None => quote!(#dagger::serde_json::json!({})),
    };

    let output_schema_tokens = match output_schema {
        Some(schema) => {
            quote!(#dagger::serde_json::from_str(#schema).unwrap_or_else(|_| #dagger::serde_json::json!({})))
        }
        None => quote!(#dagger::serde_json::json!({})),
    };

    let call = if arg_count == 1 {
        quote!(#fn_name(message.clone()).await)
    } else {
        quote!({
            let mut ctx = #dagger::pubsub::PubSubContext::new(node_id, channel, executor, cache);
            #fn_name(message.clone(), &mut ctx).await
        })
    };

    let expanded = quote! {
        #input_fn

        #[derive(Debug, Clone)]
        #fn_vis struct #struct_name;

        #[#dagger::async_trait::async_trait]
        impl #dagger::pubsub::PubSubAgent for #struct_name {
            fn name(&self) -> String {
                #agent_name.to_string()
            }

            fn description(&self) -> String {
                #agent_desc.to_string()
            }

            fn subscriptions(&self) -> Vec<String> {
                vec![#(#subs_tokens),*]
            }

            fn publications(&self) -> Vec<String> {
                vec![#(#pubs_tokens),*]
            }

            fn input_schema(&self) -> #dagger::serde_json::Value {
                #input_schema_tokens
            }

            fn output_schema(&self) -> #dagger::serde_json::Value {
                #output_schema_tokens
            }

            async fn process_message(
                &self,
                node_id: &str,
                channel: &str,
                message: &#dagger::pubsub::Message,
                executor: &mut #dagger::pubsub::PubSubExecutor,
                cache: &#dagger::dag_flow::Cache,
            ) -> #dagger::anyhow::Result<()> {
                #call?;
                Ok(())
            }
        }
    };

    TokenStream::from(expanded)
}
