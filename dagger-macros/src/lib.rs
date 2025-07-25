use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, ItemFn, Lit, Meta, MetaNameValue, parse::Parser};

/// Procedural macro for creating task agents
/// 
/// # Example
/// ```rust
/// #[task_agent(
///     name = "calculator",
///     description = "Performs calculations"
/// )]
/// async fn calculator(input: Bytes, ctx: Arc<TaskContext>) -> Result<Bytes, AgentError> {
///     // Implementation
/// }
/// ```
#[proc_macro_attribute]
pub fn task_agent(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input_fn = parse_macro_input!(item as ItemFn);
    
    // Parse attributes manually
    let parser = syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated;
    let attr_args = match parser.parse(attr.clone()) {
        Ok(args) => args,
        Err(e) => return TokenStream::from(syn::Error::to_compile_error(&e)),
    };
    
    // Extract attributes
    let mut name = String::new();
    let mut description = String::new();
    
    for meta in attr_args {
        match meta {
            Meta::NameValue(MetaNameValue { path, value, .. }) => {
                if path.is_ident("name") {
                    if let syn::Expr::Lit(expr_lit) = value {
                        if let Lit::Str(lit) = &expr_lit.lit {
                            name = lit.value();
                        }
                    }
                } else if path.is_ident("description") {
                    if let syn::Expr::Lit(expr_lit) = value {
                        if let Lit::Str(lit) = &expr_lit.lit {
                            description = lit.value();
                        }
                    }
                }
            }
            _ => {}
        }
    }
    
    if name.is_empty() {
        return TokenStream::from(quote! {
            compile_error!("task_agent requires a 'name' attribute");
        });
    }
    
    let fn_name = &input_fn.sig.ident;
    let fn_vis = &input_fn.vis;
    let struct_name = quote::format_ident!("{}Agent", fn_name);
    let agent_id = const_hash16(&name);
    
    let output = quote! {
        #input_fn
        
        #[derive(Clone)]
        #fn_vis struct #struct_name;
        
        impl #struct_name {
            pub const AGENT_ID: u16 = #agent_id;
            pub const NAME: &'static str = #name;
            pub const DESCRIPTION: &'static str = #description;
        }
        
        #[async_trait::async_trait]
        impl task_core::executor::Agent for #struct_name {
            async fn execute(
                &self,
                input: bytes::Bytes,
                ctx: std::sync::Arc<task_core::executor::TaskContext>
            ) -> Result<bytes::Bytes, task_core::model::AgentError> {
                #fn_name(input, ctx).await
            }
        }
        
        // Auto-registration using linkme
        #[linkme::distributed_slice(task_core::AGENTS)]
        static AGENT_REGISTRATION: fn(&mut task_core::executor::AgentRegistry) = |registry| {
            registry.register(
                #struct_name::AGENT_ID,
                #struct_name::NAME,
                std::sync::Arc::new(#struct_name)
            ).expect("Failed to register agent");
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

/// Original action macro for backward compatibility with existing DAG code
#[proc_macro_attribute]
pub fn action(attr: TokenStream, item: TokenStream) -> TokenStream {
    // Keep the original implementation for existing DAG flow code
    let input = parse_macro_input!(item as ItemFn);
    let fn_name = &input.sig.ident;
    let fn_name_str = fn_name.to_string();
    let fn_vis = &input.vis;
    let struct_name = syn::Ident::new(&format!("__{}Action", fn_name_str), fn_name.span());
    let static_name = syn::Ident::new(&fn_name_str.to_uppercase(), fn_name.span());
    
    // Parse attributes
    let parser = syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated;
    let attr_args = match parser.parse(attr) {
        Ok(args) => args,
        Err(e) => return TokenStream::from(syn::Error::to_compile_error(&e)),
    };
    
    let mut description = "No description provided".to_string();
    
    for meta in attr_args {
        match meta {
            Meta::NameValue(MetaNameValue { path, value, .. }) => {
                if path.is_ident("description") {
                    if let syn::Expr::Lit(expr_lit) = value {
                        if let Lit::Str(lit) = &expr_lit.lit {
                            description = lit.value();
                        }
                    }
                }
            }
            _ => {}
        }
    }
    
    let schema = quote! {
        serde_json::json!({
            "type": "object",
            "description": #description,
            "properties": {},
            "returns": { "type": "object" }
        })
    };
    
    let expanded = quote! {
        #input
        
        #[derive(Debug, Clone)]
        pub struct #struct_name;
        
        impl crate::dag_flow::Action for #struct_name {
            fn name(&self) -> String {
                #fn_name_str.to_string()
            }
            
            fn description(&self) -> String {
                #description.to_string()
            }
            
            fn schema(&self) -> serde_json::Value {
                #schema
            }
            
            fn get_action(&self) -> crate::dag_flow::NodeAction {
                crate::dag_flow::NodeAction::Function(std::sync::Arc::new(
                    |executor, node, cache| {
                        Box::pin(#fn_name(executor, node, cache))
                    }
                ))
            }
        }
        
        #[linkme::distributed_slice(crate::registry::ACTIONS)]
        #fn_vis static #static_name: crate::registry::RegistryEntry = crate::registry::RegistryEntry {
            name: #fn_name_str,
            factory: || Box::new(#struct_name),
        };
    };
    
    TokenStream::from(expanded)
}