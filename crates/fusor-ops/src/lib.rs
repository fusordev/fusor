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
    };
    expanded.into()
}
