//! `#[derive(SynthModuleParams)]` — synthesizes a module's param handling from its typed struct.
//!
//! A module declares its config as a plain struct with `#[param(...)]` field attributes; this
//! derive generates the `SynthModuleParams` impl (in `synth-core`) so the module never hand-writes
//! the descriptor list or the YAML-dict → struct conversion. The struct is the single source of
//! truth: a field's name is the param name, its type picks the `ParamKind`, and its `default`
//! attribute feeds both the descriptor default and the conversion fallback.
//!
//! Generated paths reference `::synth_core::…`, so `synth-core` itself adds
//! `extern crate self as synth_core;` and external module crates simply depend on `synth-core`.

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, Data, DeriveInput, Expr, Fields, LitStr, Type};

#[proc_macro_derive(SynthModuleParams, attributes(param))]
pub fn derive_synth_module_params(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = input.ident;

    let fields = match input.data {
        Data::Struct(s) => match s.fields {
            Fields::Named(named) => named.named,
            _ => return err(&name, "SynthModuleParams requires a struct with named fields"),
        },
        _ => return err(&name, "SynthModuleParams can only be derived for structs"),
    };

    let mut descs = Vec::new();
    let mut inits = Vec::new();

    for field in fields {
        let ident = field.ident.expect("named field");
        let pname = ident.to_string();
        let ty = field.ty;

        let mut label: Option<String> = None;
        let mut default: Option<Expr> = None;
        let mut min: Option<Expr> = None;
        let mut max: Option<Expr> = None;

        for attr in &field.attrs {
            if !attr.path().is_ident("param") {
                continue;
            }
            let parsed = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("label") {
                    let s: LitStr = meta.value()?.parse()?;
                    label = Some(s.value());
                } else if meta.path.is_ident("default") {
                    default = Some(meta.value()?.parse()?);
                } else if meta.path.is_ident("min") {
                    min = Some(meta.value()?.parse()?);
                } else if meta.path.is_ident("max") {
                    max = Some(meta.value()?.parse()?);
                } else {
                    return Err(meta.error("unknown `param` attribute key (expected label/default/min/max)"));
                }
                Ok(())
            });
            if let Err(e) = parsed {
                return e.to_compile_error().into();
            }
        }

        let label = label.unwrap_or_else(|| humanize(&pname));

        let (kind, default_value, fallback) = match last_segment(&ty).as_deref() {
            Some("f32") | Some("f64") => {
                let d = default.unwrap_or_else(|| syn::parse_quote!(0.0));
                let lo = min.map(|e| quote!(#e as f32)).unwrap_or_else(|| quote!(f32::MIN));
                let hi = max.map(|e| quote!(#e as f32)).unwrap_or_else(|| quote!(f32::MAX));
                (
                    quote!(::synth_core::module::ParamKind::Float { min: #lo, max: #hi }),
                    quote!(::synth_core::model::ParamValue::Float((#d) as f64)),
                    quote!((#d) as #ty),
                )
            }
            Some("i64") => {
                let d = default.unwrap_or_else(|| syn::parse_quote!(0));
                let lo = min.map(|e| quote!(#e)).unwrap_or_else(|| quote!(i64::MIN));
                let hi = max.map(|e| quote!(#e)).unwrap_or_else(|| quote!(i64::MAX));
                (
                    quote!(::synth_core::module::ParamKind::Int { min: #lo, max: #hi }),
                    quote!(::synth_core::model::ParamValue::Int((#d) as i64)),
                    quote!((#d) as #ty),
                )
            }
            Some("bool") => {
                let d = default.unwrap_or_else(|| syn::parse_quote!(false));
                (
                    quote!(::synth_core::module::ParamKind::Bool),
                    quote!(::synth_core::model::ParamValue::Bool(#d)),
                    quote!(#d),
                )
            }
            other => {
                let msg = format!(
                    "SynthModuleParams: unsupported field type `{}` (expected f32/f64/i64/bool)",
                    other.unwrap_or("<non-path>")
                );
                return syn::Error::new_spanned(&ty, msg).to_compile_error().into();
            }
        };

        descs.push(quote! {
            ::synth_core::module::ParamDesc {
                name: #pname.into(),
                label: #label.into(),
                kind: #kind,
                default: #default_value,
            }
        });
        inits.push(quote! {
            #ident: values
                .get(#pname)
                .and_then(<#ty as ::synth_core::module::FromParamValue>::from_param_value)
                .unwrap_or_else(|| #fallback),
        });
    }

    quote! {
        impl ::synth_core::module::SynthModuleParams for #name {
            fn param_descs() -> ::std::vec::Vec<::synth_core::module::ParamDesc> {
                ::std::vec![ #(#descs),* ]
            }
            fn from_values(values: &::synth_core::model::Params) -> Self {
                Self { #(#inits)* }
            }
        }
    }
    .into()
}

/// The last identifier of a path type (e.g. `f32`), or `None` for non-path types.
fn last_segment(ty: &Type) -> Option<String> {
    match ty {
        Type::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()),
        _ => None,
    }
}

/// `"cutoff_hz"` -> `"Cutoff_hz"` — a readable default label when none is given.
fn humanize(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn err(spanned: &syn::Ident, msg: &str) -> TokenStream {
    syn::Error::new_spanned(spanned, msg).to_compile_error().into()
}
