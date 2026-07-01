//! Helpers for source-level CLI surface walks.

use std::collections::BTreeSet;

use syn::{Expr, ExprLit, Fields, Lit, Meta, MetaNameValue};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flag {
    pub long: Option<String>,
    pub short: Option<char>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VariantShape {
    Unit,
    Tuple,
    Named,
}

pub fn parse_file(body: &str, rel_path: &str) -> Result<syn::File, String> {
    syn::parse_file(body).map_err(|e| format!("{rel_path} syn parse: {e}"))
}

pub fn enum_variant_names(
    file: &syn::File,
    enum_name: &str,
    rel_path: &str,
) -> Result<BTreeSet<String>, String> {
    let item = enum_item(file, enum_name, rel_path)?;
    let mut out = BTreeSet::new();
    for variant in &item.variants {
        let name = command_name_for_variant(variant, rel_path)?
            .unwrap_or_else(|| ident_to_kebab(&variant.ident));
        out.insert(name);
    }
    Ok(out)
}

pub fn enum_variant_flags(
    file: &syn::File,
    enum_name: &str,
    variant_name: &str,
    rel_path: &str,
) -> Result<Vec<Flag>, String> {
    let item = enum_item(file, enum_name, rel_path)?;
    for variant in &item.variants {
        let name = command_name_for_variant(variant, rel_path)?
            .unwrap_or_else(|| ident_to_kebab(&variant.ident));
        if name == variant_name {
            return flags_from_fields(&variant.fields, rel_path, &variant.ident.to_string());
        }
    }
    Ok(Vec::new())
}

pub fn enum_variant_shape(
    file: &syn::File,
    enum_name: &str,
    variant_name: &str,
    rel_path: &str,
) -> Result<VariantShape, String> {
    let item = enum_item(file, enum_name, rel_path)?;
    for variant in &item.variants {
        let name = command_name_for_variant(variant, rel_path)?
            .unwrap_or_else(|| ident_to_kebab(&variant.ident));
        if name == variant_name {
            return Ok(match &variant.fields {
                Fields::Unit => VariantShape::Unit,
                Fields::Unnamed(_) => VariantShape::Tuple,
                Fields::Named(_) => VariantShape::Named,
            });
        }
    }
    Err(format!(
        "{rel_path} enum `{enum_name}` has no `{variant_name}` variant"
    ))
}

pub fn struct_field_names(
    file: &syn::File,
    struct_name: &str,
    rel_path: &str,
) -> Result<BTreeSet<String>, String> {
    let item = struct_item(file, struct_name, rel_path)?;
    let Fields::Named(fields) = &item.fields else {
        return Err(format!(
            "{rel_path} struct `{struct_name}` has no named fields"
        ));
    };
    Ok(fields
        .named
        .iter()
        .filter_map(|field| field.ident.as_ref().map(std::string::ToString::to_string))
        .collect())
}

pub fn struct_flags(
    file: &syn::File,
    struct_name: &str,
    rel_path: &str,
) -> Result<Vec<Flag>, String> {
    let item = struct_item(file, struct_name, rel_path)?;
    flags_from_named_fields(&item.fields, rel_path, struct_name)
}

pub fn struct_long_flags(
    file: &syn::File,
    struct_name: &str,
    rel_path: &str,
) -> Result<BTreeSet<String>, String> {
    Ok(struct_flags(file, struct_name, rel_path)?
        .into_iter()
        .filter_map(|flag| flag.long)
        .collect())
}

pub fn flag_from_arg_attr(
    attr: &syn::Attribute,
    field_name: &str,
    rel_path: &str,
    owner: &str,
) -> Result<Option<Flag>, String> {
    arg_flag(attr, field_name, rel_path, owner)
}

pub fn field_requires(
    file: &syn::File,
    struct_name: &str,
    field_name: &str,
    required: &str,
    rel_path: &str,
) -> Result<bool, String> {
    let item = struct_item(file, struct_name, rel_path)?;
    let Fields::Named(fields) = &item.fields else {
        return Err(format!(
            "{rel_path} struct `{struct_name}` has no named fields"
        ));
    };
    let field = fields
        .named
        .iter()
        .find(|field| {
            field
                .ident
                .as_ref()
                .is_some_and(|ident| ident == field_name)
        })
        .ok_or_else(|| format!("{rel_path} struct `{struct_name}` has no `{field_name}` field"))?;
    for attr in &field.attrs {
        if !attr.path().is_ident("arg") {
            continue;
        }
        let meta_list = attr
            .meta
            .require_list()
            .map_err(|e| format!("{rel_path} `{struct_name}.{field_name}` arg attr: {e}"))?;
        let nested = meta_list
            .parse_args_with(syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated)
            .map_err(|e| format!("{rel_path} `{struct_name}.{field_name}` arg attr parse: {e}"))?;
        for meta in nested {
            if let Meta::NameValue(MetaNameValue {
                path,
                value:
                    Expr::Lit(ExprLit {
                        lit: Lit::Str(value),
                        ..
                    }),
                ..
            }) = meta
                && path.is_ident("requires")
                && value.value() == required
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn enum_item<'a>(
    file: &'a syn::File,
    enum_name: &str,
    rel_path: &str,
) -> Result<&'a syn::ItemEnum, String> {
    file.items
        .iter()
        .find_map(|item| match item {
            syn::Item::Enum(item) if item.ident == enum_name => Some(item),
            _ => None,
        })
        .ok_or_else(|| format!("{rel_path} no `{enum_name}` enum"))
}

fn flags_from_fields(fields: &Fields, rel_path: &str, owner: &str) -> Result<Vec<Flag>, String> {
    match fields {
        Fields::Named(_) => flags_from_named_fields(fields, rel_path, owner),
        Fields::Unit | Fields::Unnamed(_) => Ok(Vec::new()),
    }
}

fn flags_from_named_fields(
    fields: &Fields,
    rel_path: &str,
    owner: &str,
) -> Result<Vec<Flag>, String> {
    let Fields::Named(fields) = fields else {
        return Err(format!("{rel_path} `{owner}` has no named fields"));
    };
    let mut out = Vec::new();
    for field in &fields.named {
        let field_name = field
            .ident
            .as_ref()
            .map(std::string::ToString::to_string)
            .unwrap_or_default();
        for attr in &field.attrs {
            if !attr.path().is_ident("arg") {
                continue;
            }
            if let Some(flag) = arg_flag(attr, &field_name, rel_path, owner)? {
                out.push(flag);
            }
        }
    }
    Ok(out)
}

fn struct_item<'a>(
    file: &'a syn::File,
    struct_name: &str,
    rel_path: &str,
) -> Result<&'a syn::ItemStruct, String> {
    file.items
        .iter()
        .find_map(|item| match item {
            syn::Item::Struct(item) if item.ident == struct_name => Some(item),
            _ => None,
        })
        .ok_or_else(|| format!("{rel_path} no `{struct_name}` struct"))
}

fn command_name_for_variant(
    variant: &syn::Variant,
    rel_path: &str,
) -> Result<Option<String>, String> {
    for attr in &variant.attrs {
        if !attr.path().is_ident("command") && !attr.path().is_ident("clap") {
            continue;
        }
        let meta_list = attr
            .meta
            .require_list()
            .map_err(|e| format!("{rel_path} `{}` command attr: {e}", variant.ident))?;
        let nested = meta_list
            .parse_args_with(syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated)
            .map_err(|e| format!("{rel_path} `{}` command attr parse: {e}", variant.ident))?;
        for meta in nested {
            if let Meta::NameValue(MetaNameValue {
                path,
                value:
                    Expr::Lit(ExprLit {
                        lit: Lit::Str(value),
                        ..
                    }),
                ..
            }) = meta
                && path.is_ident("name")
            {
                return Ok(Some(value.value()));
            }
        }
    }
    Ok(None)
}

fn arg_flag(
    attr: &syn::Attribute,
    field_name: &str,
    rel_path: &str,
    owner: &str,
) -> Result<Option<Flag>, String> {
    let meta_list = attr
        .meta
        .require_list()
        .map_err(|e| format!("{rel_path} `{owner}.{field_name}` arg attr: {e}"))?;
    let nested = meta_list
        .parse_args_with(syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated)
        .map_err(|e| format!("{rel_path} `{owner}.{field_name}` arg attr parse: {e}"))?;
    let mut long = None;
    let mut short = None;
    for meta in nested {
        match meta {
            Meta::Path(path) if path.is_ident("long") => {
                long = Some(field_name.replace('_', "-"));
            }
            Meta::Path(path) if path.is_ident("short") => {
                short = field_name.chars().next();
            }
            Meta::NameValue(MetaNameValue {
                path,
                value:
                    Expr::Lit(ExprLit {
                        lit: Lit::Str(value),
                        ..
                    }),
                ..
            }) if path.is_ident("long") => {
                long = Some(value.value());
            }
            Meta::NameValue(MetaNameValue {
                path,
                value:
                    Expr::Lit(ExprLit {
                        lit: Lit::Char(value),
                        ..
                    }),
                ..
            }) if path.is_ident("short") => {
                short = Some(value.value());
            }
            Meta::NameValue(MetaNameValue {
                path,
                value:
                    Expr::Lit(ExprLit {
                        lit: Lit::Str(value),
                        ..
                    }),
                ..
            }) if path.is_ident("short") => {
                short = value.value().chars().next();
            }
            _ => {}
        }
    }
    if long.is_some() || short.is_some() {
        Ok(Some(Flag { long, short }))
    } else {
        Ok(None)
    }
}

fn ident_to_kebab(ident: &syn::Ident) -> String {
    let mut out = String::new();
    for (idx, ch) in ident.to_string().chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if idx > 0 {
                out.push('-');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}
