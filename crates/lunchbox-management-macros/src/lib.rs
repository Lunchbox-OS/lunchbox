//! `#[management_rpc]`: turn the `ManagementService` trait into a
//! JSON-RPC dispatcher.
//!
//! Placed as an attribute on the trait definition, it re-emits the
//! trait unchanged and adds a `dispatch_json` free function next to
//! it. The generated dispatcher matches on the RPC method name,
//! parses the JSON `params` object into per-method structs derived
//! from the method's signature, calls the trait method, and serialises
//! the result — collapsing the ~350 lines of hand-written glue that
//! previously lived in `lunchbox-ble::rpc`.
//!
//! # Per-method attributes
//!
//! Optional `#[rpc(...)]` attributes on each method tune behaviour:
//!
//! - `#[rpc(name = "wire_name")]` — override the wire method name
//!   (defaults to the Rust method name).
//! - `#[rpc(default = "expr")]` on a single parameter uses `expr` when
//!   the JSON value is missing. Written as
//!   `#[rpc(default(param_name = "lunchbox_util::now"))]`.
//! - `#[rpc(wrap_result = "field_name")]` — wrap a bare-primitive
//!   result in a `{ "field_name": <value> }` object. Used for the
//!   handful of methods where the wire form carries a named field
//!   (e.g. `delete_override` → `{ "deleted": true }`) but the trait
//!   returns just the primitive.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{
    Attribute, FnArg, GenericArgument, ItemTrait, LitStr, Pat, PathArguments, ReturnType,
    TraitItem, TraitItemFn, Type, parse_macro_input,
};

#[proc_macro_attribute]
pub fn management_rpc(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemTrait);
    let trait_name = &input.ident;

    // Every trait method that carries an `async` keyword is treated
    // as an RPC entry. Non-async items (default methods, sync helpers
    // like `subscribe_events`) are skipped.
    let mut arms: Vec<TokenStream2> = Vec::new();
    let mut schema_entries: Vec<String> = Vec::new();
    for item in &input.items {
        let TraitItem::Fn(f) = item else { continue };
        if f.sig.asyncness.is_none() {
            continue;
        }
        match build_arm(f) {
            Ok((arm, schema)) => {
                arms.push(arm);
                schema_entries.push(schema);
            }
            Err(e) => return e.to_compile_error().into(),
        }
    }

    // Re-emit the trait with the `#[rpc(...)]` attributes stripped so
    // rustc doesn't complain about unknown attributes on the actual
    // trait definition.
    let mut stripped = input.clone();
    for item in &mut stripped.items {
        if let TraitItem::Fn(f) = item {
            f.attrs.retain(|a| !a.path().is_ident("rpc"));
        }
    }

    // Assemble the schema JSON at macro time so consumers get a plain
    // `&'static str` — cheap to embed, no serde dep at the boundary.
    let schema_json = format!("{{\"methods\":[{}]}}", schema_entries.join(","));

    let expanded = quote! {
        #stripped

        /// Auto-generated JSON-RPC dispatcher over `#trait_name`.
        ///
        /// Matches `method` against the trait's async methods, parses
        /// `params` into the method's expected shape, invokes the
        /// method on `svc`, and returns the JSON-encoded result. See
        /// the `#[management_rpc]` proc-macro for details.
        pub async fn dispatch_json(
            svc: &dyn #trait_name,
            method: &str,
            params: ::serde_json::Value,
        ) -> ::std::result::Result<::serde_json::Value, crate::dispatch::RpcDispatchError> {
            match method {
                #(#arms)*
                other => Err(
                    crate::dispatch::RpcDispatchError::MethodNotFound(other.to_string()),
                ),
            }
        }

        /// Machine-readable schema for the trait's RPCs, meant for
        /// external codegen (companion-android's Kotlin client,
        /// lunchbox-webui's TypeScript client). The shape is a JSON
        /// object `{ "methods": [{ "name", "params": [...], "result": {..} }, ...] }`.
        /// Each param has `{ "name", "type", "required" }`; the result
        /// carries `{ "type", "wrap_field": <optional> }`. Types are
        /// verbatim Rust source strings — consumers map them to their
        /// own type systems.
        pub const RPC_SCHEMA_JSON: &str = #schema_json;
    };

    expanded.into()
}

// ---------------------------------------------------------------------------
// Per-method arm generation
// ---------------------------------------------------------------------------

/// Rust-source method name plus its parsed `#[rpc(...)]` overrides.
struct MethodMeta {
    wire_name: String,
    wrap_result: Option<String>,
    defaults: Vec<(String, String)>,
}

fn parse_rpc_attrs(attrs: &[Attribute], rust_name: &str) -> syn::Result<MethodMeta> {
    let mut wire_name: Option<String> = None;
    let mut wrap_result: Option<String> = None;
    let mut defaults: Vec<(String, String)> = Vec::new();

    for attr in attrs {
        if !attr.path().is_ident("rpc") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("name") {
                let value: LitStr = meta.value()?.parse()?;
                wire_name = Some(value.value());
                Ok(())
            } else if meta.path.is_ident("wrap_result") {
                let value: LitStr = meta.value()?.parse()?;
                wrap_result = Some(value.value());
                Ok(())
            } else if meta.path.is_ident("default") {
                // `default(param_name = "expr_path")` — one entry per
                // call is required so we can round-trip cleanly.
                meta.parse_nested_meta(|inner| {
                    let param = inner
                        .path
                        .get_ident()
                        .ok_or_else(|| inner.error("expected parameter name"))?
                        .to_string();
                    let expr: LitStr = inner.value()?.parse()?;
                    defaults.push((param, expr.value()));
                    Ok(())
                })
            } else {
                Err(meta.error("unknown #[rpc(...)] option"))
            }
        })?;
    }

    Ok(MethodMeta {
        wire_name: wire_name.unwrap_or_else(|| rust_name.to_string()),
        wrap_result,
        defaults,
    })
}

/// One `Param` for each of the method's non-receiver arguments.
struct Param {
    /// Identifier as written in the trait signature.
    name: syn::Ident,
    /// The type as written, minus any leading reference — we always
    /// deserialise into an owned value and re-borrow at the call site
    /// if needed.
    ty: Type,
    /// True when the trait signature took `&T`; the call site should
    /// pass `&p.name` instead of `p.name`.
    is_reference: bool,
    /// A Rust expression string that produces a default value when
    /// the JSON object omits this field. `None` means the field is
    /// required.
    default_expr: Option<String>,
}

fn build_arm(f: &TraitItemFn) -> syn::Result<(TokenStream2, String)> {
    let rust_name = f.sig.ident.to_string();
    let meta = parse_rpc_attrs(&f.attrs, &rust_name)?;
    let wire_name = &meta.wire_name;
    let method_ident = &f.sig.ident;

    // Collect params from the signature.
    let mut params: Vec<Param> = Vec::new();
    for input in f.sig.inputs.iter().skip(1) {
        // skip receiver
        let FnArg::Typed(pat_ty) = input else {
            continue;
        };
        let Pat::Ident(ident) = &*pat_ty.pat else {
            return Err(syn::Error::new_spanned(
                &pat_ty.pat,
                "expected a plain identifier parameter",
            ));
        };
        let (ty, is_reference) = match &*pat_ty.ty {
            Type::Reference(r) => ((*r.elem).clone(), true),
            other => (other.clone(), false),
        };
        let default_expr = meta
            .defaults
            .iter()
            .find(|(n, _)| n == &ident.ident.to_string())
            .map(|(_, e)| e.clone());
        params.push(Param {
            name: ident.ident.clone(),
            ty,
            is_reference,
            default_expr,
        });
    }

    let schema = build_schema_entry(
        wire_name,
        &params,
        &f.sig.output,
        meta.wrap_result.as_deref(),
    );

    // Build the per-method params struct: `struct Params { field: Ty, ... }`,
    // with `Option<Ty>` for defaulted fields so `serde` treats them as
    // missing-tolerant.
    let params_struct = if params.is_empty() {
        quote! {}
    } else {
        let fields = params.iter().map(|p| {
            let name = &p.name;
            let ty = &p.ty;
            if p.default_expr.is_some() {
                quote! {
                    #[serde(default)]
                    #name: ::std::option::Option<#ty>,
                }
            } else {
                quote! { #name: #ty, }
            }
        });
        quote! {
            #[derive(::serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Params {
                #(#fields)*
            }
        }
    };

    let parse_stmt = if params.is_empty() {
        // Ignore the params blob for zero-arg methods rather than
        // rejecting a `{}` or absent value.
        quote! {
            let _ = params;
        }
    } else {
        quote! {
            let p: Params = ::serde_json::from_value(params)
                .map_err(|e| crate::dispatch::RpcDispatchError::InvalidParams(e.to_string()))?;
        }
    };

    // Bind each param either to `p.name` or, when a default was
    // supplied, to `p.name.unwrap_or_else(...)`.
    let param_bindings = params.iter().map(|p| {
        let name = &p.name;
        if let Some(expr) = &p.default_expr {
            let expr: syn::Expr = syn::parse_str(expr).unwrap_or_else(|_| {
                syn::parse_str("::std::default::Default::default").expect("Default::default parses")
            });
            quote! { let #name = p.#name.unwrap_or_else(#expr); }
        } else {
            quote! { let #name = p.#name; }
        }
    });

    // Call arguments: `&name` for reference-typed params, `name` otherwise.
    let call_args = params.iter().map(|p| {
        let name = &p.name;
        if p.is_reference {
            quote! { &#name }
        } else {
            quote! { #name }
        }
    });

    // Decide how to unwrap the return: bare `T`, `ManagementResult<T>`,
    // or `ManagementResult<()>` / `()`.
    let call_and_wrap = build_return_handler(&f.sig.output, meta.wrap_result.as_deref())?;

    let arm = quote! {
        #wire_name => {
            #params_struct
            #parse_stmt
            #(#param_bindings)*
            let __result = svc.#method_ident(#(#call_args),*).await;
            #call_and_wrap
        }
    };
    Ok((arm, schema))
}

// ---------------------------------------------------------------------------
// Schema emission
// ---------------------------------------------------------------------------

/// Emit a single method's JSON schema entry as a source-level string.
/// We build the JSON by hand rather than pulling in serde_json because
/// this runs at proc-macro time — proc-macro crates want to keep their
/// dependency graph tight.
fn build_schema_entry(
    wire_name: &str,
    params: &[Param],
    output: &ReturnType,
    wrap_field: Option<&str>,
) -> String {
    let params_json: Vec<String> = params
        .iter()
        .map(|p| {
            format!(
                "{{\"name\":\"{}\",\"type\":\"{}\",\"required\":{}}}",
                p.name,
                json_escape(&type_to_source(&p.ty)),
                p.default_expr.is_none()
            )
        })
        .collect();

    let (result_type, wrap) = describe_result(output, wrap_field);
    let mut result_json = format!("{{\"type\":\"{}\"", json_escape(&result_type));
    if let Some(field) = wrap {
        result_json.push_str(&format!(",\"wrap_field\":\"{}\"", json_escape(&field)));
    }
    result_json.push('}');

    format!(
        "{{\"name\":\"{}\",\"params\":[{}],\"result\":{}}}",
        wire_name,
        params_json.join(","),
        result_json
    )
}

/// Turn a `syn::Type` back into its source-level form so the schema
/// carries the exact spelling from the trait — e.g. `Vec<EntryView>`,
/// `Option<DateTime<Local>>`. Consumers map these strings to their
/// target language's types.
fn type_to_source(ty: &Type) -> String {
    quote!(#ty).to_string().replace(' ', "")
}

fn describe_result(output: &ReturnType, wrap_field: Option<&str>) -> (String, Option<String>) {
    match output {
        ReturnType::Default => ("()".into(), None),
        ReturnType::Type(_, ty) => {
            // Peel a `ManagementResult<T>` wrapper so the schema
            // advertises the *successful* payload type; errors are a
            // separate concern the transport layers already model.
            let inner = strip_management_result(ty).unwrap_or_else(|| ty.as_ref().clone());
            (type_to_source(&inner), wrap_field.map(str::to_owned))
        }
    }
}

fn strip_management_result(ty: &Type) -> Option<Type> {
    let Type::Path(p) = ty else { return None };
    let last = p.path.segments.last()?;
    if last.ident != "ManagementResult" {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &last.arguments else {
        return None;
    };
    let GenericArgument::Type(inner) = args.args.first()? else {
        return None;
    };
    Some(inner.clone())
}

fn json_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Emit the tail that turns the method's return value into
/// `Ok(Value)` / `Err(RpcDispatchError)`. Handles four shapes:
///
/// - `()` / no return — encode as `Value::Null`.
/// - `ManagementResult<()>` — bubble `Err`; encode `Ok(())` as
///   `Value::Null`.
/// - `ManagementResult<T>` — bubble `Err`; encode `Ok(v)` as
///   `to_value(v)`.
/// - bare `T` — encode as `to_value(T)`.
///
/// If `wrap_result = "field"` is set, `to_value(v)` becomes
/// `serde_json::json!({ "field": v })` — used for the small set of
/// trait methods (`extend_current`, `delete_override`, `reload_config`)
/// whose wire shape wraps a bare primitive in a named field for
/// forward-compatibility.
fn build_return_handler(
    output: &ReturnType,
    wrap_field: Option<&str>,
) -> syn::Result<TokenStream2> {
    let wrap = |value: TokenStream2| -> TokenStream2 {
        match wrap_field {
            Some(field) => quote! {
                ::serde_json::json!({ #field: #value })
            },
            None => quote! {
                ::serde_json::to_value(#value)
                    .map_err(|e| crate::dispatch::RpcDispatchError::Serialization(e.to_string()))?
            },
        }
    };

    match output {
        ReturnType::Default => Ok(quote! {
            let _ = __result;
            Ok(::serde_json::Value::Null)
        }),
        ReturnType::Type(_, ty) => match classify_return(ty) {
            ReturnShape::UnitResult => Ok(quote! {
                match __result {
                    Ok(()) => Ok(::serde_json::Value::Null),
                    Err(e) => Err(crate::dispatch::RpcDispatchError::Management(e)),
                }
            }),
            ReturnShape::Result => {
                let wrapped = wrap(quote! { v });
                Ok(quote! {
                    match __result {
                        Ok(v) => Ok(#wrapped),
                        Err(e) => Err(crate::dispatch::RpcDispatchError::Management(e)),
                    }
                })
            }
            ReturnShape::Bare => {
                let wrapped = wrap(quote! { __result });
                Ok(quote! { Ok(#wrapped) })
            }
        },
    }
}

enum ReturnShape {
    /// `ManagementResult<()>` — success serialises to `Value::Null`.
    UnitResult,
    /// `ManagementResult<T>` for any non-unit `T`.
    Result,
    /// A bare `T`.
    Bare,
}

fn classify_return(ty: &Type) -> ReturnShape {
    // Look for `ManagementResult<...>`. Anything else falls through to
    // `Bare`. This is deliberately narrow — trait authors who need
    // stranger shapes can lower them into a wrapper before returning.
    let Type::Path(p) = ty else {
        return ReturnShape::Bare;
    };
    let Some(last) = p.path.segments.last() else {
        return ReturnShape::Bare;
    };
    if last.ident != "ManagementResult" {
        return ReturnShape::Bare;
    }
    let PathArguments::AngleBracketed(args) = &last.arguments else {
        return ReturnShape::Bare;
    };
    let Some(GenericArgument::Type(inner)) = args.args.first() else {
        return ReturnShape::Bare;
    };
    // Distinguish `ManagementResult<()>` from `ManagementResult<T>` so
    // the unit case doesn't try to `serde_json::to_value(())` (which
    // would work — it produces null — but this is clearer at the
    // dispatch site).
    if let Type::Tuple(t) = inner
        && t.elems.is_empty()
    {
        return ReturnShape::UnitResult;
    }
    ReturnShape::Result
}
