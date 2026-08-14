//! Proc-macro declarations for Project Fusor host operations (§5.1).
//!
//! `#[op]` marks a synchronous host operation; `#[op(async)]` marks an async
//! one. The op name defaults to the function's original snake_case name and
//! can be overridden with `#[op(name = "...")]`. The macro keeps the original
//! function untouched and additionally emits a hidden declaration accessor
//! (`__fusor_op_declaration_<name>`) that the host registry consumes at
//! assembly time. Expansions reference only the public APIs of `fusor-host`
//! and `fusor-runtime` (§5.7).

use proc_macro::TokenStream;
use quote::quote;
use syn::{Attribute, ItemFn, LitStr, parse_macro_input, parse_quote};

struct OpAttribute {
    is_async: bool,
    name: Option<String>,
}

fn parse_op_attributes(attributes: &[Attribute]) -> Result<OpAttribute, syn::Error> {
    let mut parsed = OpAttribute {
        is_async: false,
        name: None,
    };
    for attribute in attributes {
        if !attribute.path().is_ident("op") {
            continue;
        }
        attribute.parse_nested_meta(|meta| {
            if meta.path.is_ident("async") {
                parsed.is_async = true;
                return Ok(());
            }
            if meta.path.is_ident("name") {
                let value = meta.value()?;
                let name: LitStr = value.parse()?;
                parsed.name = Some(name.value());
                return Ok(());
            }
            Err(meta.error("unsupported #[op] option; expected `async` or `name = \"...\"`"))
        })?;
    }
    Ok(parsed)
}

fn parameter_type_names(function: &ItemFn) -> Vec<String> {
    function
        .sig
        .inputs
        .iter()
        .map(|argument| {
            let syn::FnArg::Typed(typed) = argument else {
                return "<receiver>".to_owned();
            };
            let ty = &typed.ty;
            quote!(#ty).to_string().replace(' ', "")
        })
        .collect()
}

/// Expands `#[op]`, `#[op(async)]`, and `#[op(name = "...")]`.
#[proc_macro_attribute]
pub fn op(attribute: TokenStream, item: TokenStream) -> TokenStream {
    let function = parse_macro_input!(item as ItemFn);
    let attribute_tokens = proc_macro2::TokenStream::from(attribute);
    let synthetic: Attribute = parse_quote!(#[op(#attribute_tokens)]);
    let options = match parse_op_attributes(std::slice::from_ref(&synthetic)) {
        Ok(options) => options,
        Err(error) => return error.into_compile_error().into(),
    };
    let visibility = &function.vis;
    let function_name = &function.sig.ident;
    let op_name = options.name.unwrap_or_else(|| function_name.to_string());
    let is_async = options.is_async;
    let parameter_types: Vec<LitStr> = parameter_type_names(&function)
        .iter()
        .map(|name| LitStr::new(name, function_name.span()))
        .collect();
    let accessor_name = syn::Ident::new(
        &format!("__fusor_op_declaration_{function_name}"),
        function_name.span(),
    );
    let call_name = syn::Ident::new(
        &format!("__fusor_op_call_{function_name}"),
        function_name.span(),
    );

    // Parameter names and types for the generated calling glue.
    let parameters: Vec<(&syn::Ident, &syn::Type)> = function
        .sig
        .inputs
        .iter()
        .filter_map(|argument| match argument {
            syn::FnArg::Typed(typed) => match &*typed.pat {
                syn::Pat::Ident(identifier) => Some((&identifier.ident, &*typed.ty)),
                _ => None,
            },
            syn::FnArg::Receiver(_) => None,
        })
        .collect();
    let parameter_names: Vec<&syn::Ident> = parameters.iter().map(|(name, _)| *name).collect();

    // Per-parameter deserialization: `ResourceId` parameters resolve through
    // the installed resource table (§5.6, §5.8) and everything else through
    // the serde bridge.
    let parameter_bindings: Vec<proc_macro2::TokenStream> = parameters
        .iter()
        .enumerate()
        .map(|(index, (name, ty))| {
            let index_literal = syn::Index::from(index);
            let type_text = quote!(#ty).to_string().replace(' ', "");
            if type_text == "JsValue" || type_text == "fusor_runtime::JsValue" {
                // A JsValue parameter passes through untouched (function
                // callbacks, raw values the op inspects itself).
                quote! {
                    let #name: #ty = match arguments.next() {
                        ::std::option::Option::Some(value) => value.clone(),
                        ::std::option::Option::None => {
                            return ::std::result::Result::Err(
                                ::fusor_host::ops::op_error_value(
                                    ctx,
                                    ::fusor_host::ops::OpError::type_error(
                                        #index_literal,
                                        "missing argument",
                                    ),
                                ),
                            );
                        }
                    };
                }
            } else if type_text.ends_with("ResourceId") {
                quote! {
                    let #name: #ty = match arguments.next() {
                        ::std::option::Option::Some(value) => {
                            let Some(raw) = value.as_u32().ok().flatten() else {
                                return ::std::result::Result::Err(
                                    ::fusor_host::ops::op_error_value(
                                        ctx,
                                        ::fusor_host::ops::OpError::type_error(
                                            #index_literal,
                                            "expected a resource id Number",
                                        ),
                                    ),
                                );
                            };
                            match ::fusor_host::ops::lookup_resource(raw) {
                                ::std::option::Option::Some(_resource) => {
                                    ::fusor_host::ops::ResourceId::from_u32(raw)
                                }
                                ::std::option::Option::None => {
                                    return ::std::result::Result::Err(
                                        ::fusor_host::ops::op_error_value(
                                            ctx,
                                            ::fusor_host::ops::OpError::type_error(
                                                #index_literal,
                                                "resource not found",
                                            ),
                                        ),
                                    );
                                }
                            }
                        }
                        ::std::option::Option::None => {
                            return ::std::result::Result::Err(
                                ::fusor_host::ops::op_error_value(
                                    ctx,
                                    ::fusor_host::ops::OpError::type_error(
                                        #index_literal,
                                        "missing argument",
                                    ),
                                ),
                            );
                        }
                    };
                }
            } else {
                quote! {
                    let #name: #ty = match arguments.next() {
                        ::std::option::Option::Some(value) => {
                            match ::serde::Deserialize::deserialize(
                                ::fusor_host::ops::JsValueDeserializer::new(ctx, value, #index_literal),
                            ) {
                                ::std::result::Result::Ok(value) => value,
                                ::std::result::Result::Err(error) => {
                                    return ::std::result::Result::Err(
                                        ::fusor_host::ops::op_error_value(
                                            ctx,
                                            ::fusor_host::ops::OpError::type_error(
                                                error.parameter,
                                                error.message,
                                            ),
                                        ),
                                    );
                                }
                            }
                        }
                        ::std::option::Option::None => {
                            return ::std::result::Result::Err(
                                ::fusor_host::ops::op_error_value(
                                    ctx,
                                    ::fusor_host::ops::OpError::type_error(
                                        #index_literal,
                                        "missing argument",
                                    ),
                                ),
                            );
                        }
                    };
                }
            }
        })
        .collect();

    // The generated glue deserializes each argument through the serde bridge,
    // invokes the op function, and serializes the result back; `OpError`s
    // become thrown JavaScript errors of their class (§5.3).
    let glue = if options.is_async {
        quote! {
            #[doc(hidden)]
            #[allow(non_snake_case)]
            #visibility fn #call_name(
                ctx: &mut ::fusor_runtime::Context<'_>,
                call: ::fusor_runtime::HostCall,
            ) -> ::std::result::Result<::fusor_runtime::JsValue, ::fusor_runtime::JsValue> {
                let mut arguments = call.arguments().iter();
                #(#parameter_bindings)*
                // ECMA-262 host Promise: settle it when the spawned future
                // completes and the owner task polls the completion channel.
                let (promise, resolver) = match ctx.new_promise() {
                    ::std::result::Result::Ok(pair) => pair,
                    ::std::result::Result::Err(error) => {
                        return ::std::result::Result::Err(
                            ::fusor_host::ops::op_error_value(
                                ctx,
                                ::fusor_host::ops::OpError::new(error.to_string()),
                            ),
                        );
                    }
                };
                let future = async move { #function_name(#(#parameter_names),*).await };
                match ::fusor_host::ops::spawn_op(resolver, future) {
                    ::std::result::Result::Ok(()) => {
                        ::std::result::Result::Ok(promise.as_value())
                    }
                    ::std::result::Result::Err(error) => {
                        ::std::result::Result::Err(
                            ::fusor_host::ops::op_error_value(
                                ctx,
                                ::fusor_host::ops::OpError::new(error.to_string()),
                            ),
                        )
                    }
                }
            }
        }
    } else {
        quote! {
            #[doc(hidden)]
            #[allow(non_snake_case)]
            #visibility fn #call_name(
                ctx: &mut ::fusor_runtime::Context<'_>,
                call: ::fusor_runtime::HostCall,
            ) -> ::std::result::Result<::fusor_runtime::JsValue, ::fusor_runtime::JsValue> {
                let mut arguments = call.arguments().iter();
                #(#parameter_bindings)*
                match #function_name(#(#parameter_names),*) {
                    ::std::result::Result::Ok(value) => {
                        ::fusor_host::ops::serialize_value(ctx, &value)
                    }
                    ::std::result::Result::Err(error) => {
                        ::std::result::Result::Err(::fusor_host::ops::op_error_value(ctx, error))
                    }
                }
            }
        }
    };

    let expanded = quote! {
        #function

        #[doc(hidden)]
        #[allow(non_snake_case)]
        #visibility fn #accessor_name() -> ::fusor_host::ops::OpDeclaration {
            ::fusor_host::ops::OpDeclaration {
                name: #op_name,
                parameter_types: &[#(#parameter_types),*],
                is_async: #is_async,
            }
        }

        #glue
    };
    expanded.into()
}
