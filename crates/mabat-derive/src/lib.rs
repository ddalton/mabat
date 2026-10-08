//! `#[derive(View)]` for Mabat.
//!
//! Use it through the `mabat` crate, which documents the attributes.

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{format_ident, quote};
use syn::ext::IdentExt;
use syn::spanned::Spanned;
use syn::{Data, DataEnum, DeriveInput, Fields, GenericArgument, Ident, LitStr, PathArguments, Type};

/// Derive a Mabat view.
///
/// On a struct, either `#[view(table = "...", key = "...")]` for a view loaded from a
/// table (`key` defaults to `id`), or `#[view(embedded)]` for a struct stored in columns of
/// the table of the view that contains it.
///
/// On an enum, `#[view(tag = "...")]` names the tag column, whose value selects the
/// variant. The data of the variants is stored in columns of the containing view's table
/// (`strategy = "tag"`, the default), or in a table per variant whose key is the key of the
/// containing view (`strategy = "table_per_variant"`). `lenient` allows non-null columns
/// of other variants. On variants:
/// - `#[view(tag_value = "...")]`: the tag value, the variant name by default
/// - `#[view(table = "...", key = "...")]`: the variant's table and its key column (`id` by
///   default), for `table_per_variant`
///
/// On fields, including the fields of variants:
/// - `#[view(column = "...")]`: the column, when it differs from the field name
/// - `#[view(child(fk = "...", order_by = "a, b desc"))]`: a `Vec` loaded by a child query
/// - `#[view(to_one(fk = "..."))]`: a reference to another view, `Option` if the foreign
///   key is nullable
/// - `#[view(embed)]` or `#[view(embed(prefix = "..."))]`: an embedded struct or an enum,
///   with an optional common column prefix
/// - `#[view(json)]`: a column decoded from JSON with `serde`
///
/// On any of these, `#[view(databases = "postgres, sqlite")]` limits the decoders to the
/// listed databases, for a view whose field types not every enabled database can decode.
/// By default a view is decoded on each database whose feature is enabled.
#[proc_macro_derive(View, attributes(view))]
pub fn derive_view(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as DeriveInput);
    expand(input).unwrap_or_else(syn::Error::into_compile_error).into()
}

enum Target {
    View { table: String, key: String },
    Embedded,
    Sum { tag: String, strategy: Strategy, lenient: bool },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Strategy {
    Tag,
    TablePerVariant,
}

enum FieldSpec {
    Column { column: String },
    Json { column: String },
    Embed { prefix: String, ty: Type },
    Child(Box<ChildSpec>),
    ToOne { fk: String, optional: bool, target: Type, form: Form },
}

struct ChildSpec {
    form: Form,
    fk: String,
    order_by: Vec<(String, bool)>,
    /// The view of the elements.
    element: Type,
    /// The key type of a map collection.
    map_key_type: Option<Type>,
    /// `(table, target)`
    through: Option<(String, String)>,
    index: Option<String>,
    map_key: Option<String>,
    recursion: Option<RecursionSpec>,
}

/// How a related view is held.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Form {
    /// An owned value, `T`.
    Owned,
    /// A value shared by everything that references the same entity, `Arc<T>`.
    Shared,
    /// A reference into a graph, `Ref<T>`.
    Graph,
}

/// The form of `ty` and the view it holds: `Arc<T>`, `Ref<T>` or `T`.
fn form_of(ty: &Type) -> (Form, Type) {
    if let Some(inner) = generic_argument(ty, "Arc") {
        (Form::Shared, inner.clone())
    } else if let Some(inner) = generic_argument(ty, "Ref") {
        (Form::Graph, inner.clone())
    } else {
        (Form::Owned, ty.clone())
    }
}

enum RecursionSpec {
    Depth(u32),
    Cte(Option<u32>),
}

/// How a field is named: a named field, or the position of a tuple field.
enum Member {
    Named(Ident),
    Unnamed,
}

struct ViewField {
    vis: syn::Visibility,
    member: Member,
    /// The path segment: the field name, or the position of a tuple field.
    name: String,
    span: Span,
    ty: Type,
    spec: FieldSpec,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Style {
    Unit,
    Named,
    Tuple,
}

struct VariantSpec {
    ident: Ident,
    tag_value: String,
    table: Option<(String, String)>,
    style: Style,
    fields: Vec<ViewField>,
}

/// The name of the facade crate that the generated code refers to.
const FACADE: &str = "mabat";

/// The path of the facade crate as the deriving crate depends on it, which may have renamed it.
fn facade() -> syn::Result<TokenStream2> {
    match proc_macro_crate::crate_name(FACADE) {
        // The facade's own examples and integration tests, which use it by its name
        Ok(proc_macro_crate::FoundCrate::Itself) => {
            let ident = Ident::new(&FACADE.replace('-', "_"), Span::call_site());
            Ok(quote! { ::#ident })
        }
        Ok(proc_macro_crate::FoundCrate::Name(name)) => {
            let ident = Ident::new(&name, Span::call_site());
            Ok(quote! { ::#ident })
        }
        Err(e) => Err(syn::Error::new(
            Span::call_site(),
            format!("`#[derive(View)]` needs the `{FACADE}` crate as a dependency: {e}"),
        )),
    }
}

fn expand(input: DeriveInput) -> syn::Result<TokenStream2> {
    let facade = facade()?;
    let items = expand_items(input)?;
    // The generated code refers to the facade as `__mabat`, whatever its name in Cargo.toml
    Ok(quote! {
        const _: () = {
            use #facade as __mabat;
            #items
        };
    })
}

fn expand_items(input: DeriveInput) -> syn::Result<TokenStream2> {
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new(input.generics.span(), "a view cannot have generic parameters"));
    }
    let (target, databases) = parse_target(&input)?;
    match (&input.data, target) {
        (Data::Struct(data), Target::View { table, key }) => {
            expand_view(&input.ident, &table, &key, &struct_fields(&input.ident, &data.fields)?, databases)
        }
        (Data::Struct(data), Target::Embedded) => {
            expand_embedded(&input.ident, &struct_fields(&input.ident, &data.fields)?, databases)
        }
        (Data::Enum(data), Target::Sum { tag, strategy, lenient }) => {
            let variants = parse_variants(data, strategy)?;
            expand_sum(&input.ident, &tag, strategy, lenient, &variants, databases)
        }
        (Data::Union(_), _) => Err(syn::Error::new(input.ident.span(), "a union cannot derive View")),
        _ => unreachable!("parse_target matches the kind of item"),
    }
}

fn struct_fields(ident: &Ident, fields: &Fields) -> syn::Result<Vec<ViewField>> {
    match fields {
        Fields::Named(named) => parse_fields(&named.named),
        _ => Err(syn::Error::new(ident.span(), "a view needs to be a struct with named fields")),
    }
}

fn parse_target(input: &DeriveInput) -> syn::Result<(Target, Databases)> {
    let is_enum = matches!(input.data, Data::Enum(_));
    let mut databases = Databases::ALL;
    let mut table = None;
    let mut key = None;
    let mut embedded = false;
    let mut tag = None;
    let mut strategy = None;
    let mut lenient = false;
    for attr in input.attrs.iter().filter(|a| a.path().is_ident("view")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("databases") {
                databases = Databases::parse(&meta.value()?.parse::<LitStr>()?)?;
            } else if is_enum {
                if meta.path.is_ident("tag") {
                    tag = Some(meta.value()?.parse::<LitStr>()?.value());
                } else if meta.path.is_ident("strategy") {
                    let lit = meta.value()?.parse::<LitStr>()?;
                    strategy = Some(match lit.value().as_str() {
                        "tag" => Strategy::Tag,
                        "table_per_variant" => Strategy::TablePerVariant,
                        _ => return Err(syn::Error::new(lit.span(), "expected `tag` or `table_per_variant`")),
                    });
                } else if meta.path.is_ident("lenient") {
                    lenient = true;
                } else {
                    return Err(meta.error(
                        "unknown view attribute for an enum, expected `tag`, `strategy`, `lenient` or `databases`",
                    ));
                }
            } else if meta.path.is_ident("table") {
                table = Some(meta.value()?.parse::<LitStr>()?.value());
            } else if meta.path.is_ident("key") {
                key = Some(meta.value()?.parse::<LitStr>()?.value());
            } else if meta.path.is_ident("embedded") {
                embedded = true;
            } else {
                return Err(meta.error("unknown view attribute, expected `table`, `key`, `embedded` or `databases`"));
            }
            Ok(())
        })?;
    }

    if is_enum {
        let tag = tag.ok_or_else(|| {
            syn::Error::new(input.ident.span(), "missing `#[view(tag = \"...\")]`, the column that names the variant")
        })?;
        return Ok((Target::Sum { tag, strategy: strategy.unwrap_or(Strategy::Tag), lenient }, databases));
    }
    let target = match (embedded, table) {
        (true, None) if key.is_none() => Target::Embedded,
        (true, _) => return Err(syn::Error::new(input.ident.span(), "an embedded struct has no `table` or `key`")),
        (false, Some(table)) => Target::View { table, key: key.unwrap_or_else(|| "id".to_string()) },
        (false, None) => {
            return Err(syn::Error::new(
                input.ident.span(),
                "missing `#[view(table = \"...\")]`, or `#[view(embedded)]` for an embedded struct",
            ));
        }
    };
    Ok((target, databases))
}

fn parse_variants(data: &DataEnum, strategy: Strategy) -> syn::Result<Vec<VariantSpec>> {
    let mut variants: Vec<VariantSpec> = Vec::new();
    for variant in &data.variants {
        let mut tag_value = None;
        let mut table = None;
        let mut key = None;
        for attr in variant.attrs.iter().filter(|a| a.path().is_ident("view")) {
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("tag_value") {
                    tag_value = Some(meta.value()?.parse::<LitStr>()?.value());
                } else if meta.path.is_ident("table") {
                    table = Some(meta.value()?.parse::<LitStr>()?.value());
                } else if meta.path.is_ident("key") {
                    key = Some(meta.value()?.parse::<LitStr>()?.value());
                } else {
                    return Err(meta.error("unknown variant attribute, expected `tag_value`, `table` or `key`"));
                }
                Ok(())
            })?;
        }

        let (style, fields) = match &variant.fields {
            Fields::Unit => (Style::Unit, Vec::new()),
            Fields::Named(named) => (Style::Named, parse_fields(&named.named)?),
            Fields::Unnamed(unnamed) => (Style::Tuple, parse_fields(&unnamed.unnamed)?),
        };

        let span = variant.ident.span();
        let table = match (strategy, style, table) {
            (Strategy::Tag, _, table) => {
                if table.is_some() || key.is_some() {
                    return Err(syn::Error::new(
                        span,
                        "`table` and `key` need `strategy = \"table_per_variant\"` on the enum",
                    ));
                }
                None
            }
            (Strategy::TablePerVariant, Style::Unit, table) if table.is_some() || key.is_some() => {
                return Err(syn::Error::new(span, "a unit variant has no data, so it has no `table` or `key`"));
            }
            (Strategy::TablePerVariant, Style::Unit, None) => None,
            (Strategy::TablePerVariant, _, None) => {
                return Err(syn::Error::new(span, "missing `#[view(table = \"...\")]` for the variant's data"));
            }
            (Strategy::TablePerVariant, _, Some(table)) => Some((table, key.unwrap_or_else(|| "id".to_string()))),
        };

        if strategy == Strategy::Tag {
            for field in &fields {
                if matches!(field.spec, FieldSpec::Child(_) | FieldSpec::ToOne { .. }) {
                    return Err(syn::Error::new(
                        field.span,
                        "a variant stored in columns can only contain columns, embedded values and json fields; \
                         use `strategy = \"table_per_variant\"` for child collections and references",
                    ));
                }
            }
        }

        for field in &fields {
            let graph = match &field.spec {
                FieldSpec::Child(child) => child.form == Form::Graph,
                FieldSpec::ToOne { form, .. } => *form == Form::Graph,
                _ => false,
            };
            if graph {
                return Err(syn::Error::new(
                    field.span,
                    "references into a graph (`Ref<T>`) are not supported in variants",
                ));
            }
        }

        let tag_value = tag_value.unwrap_or_else(|| variant.ident.unraw().to_string());
        if variants.iter().any(|v| v.tag_value == tag_value) {
            return Err(syn::Error::new(span, format!("two variants have the tag value {tag_value:?}")));
        }
        variants.push(VariantSpec { ident: variant.ident.clone(), tag_value, table, style, fields });
    }
    if variants.is_empty() {
        return Err(syn::Error::new(Span::call_site(), "an enum view needs at least one variant"));
    }
    Ok(variants)
}

fn parse_fields(fields: &syn::punctuated::Punctuated<syn::Field, syn::token::Comma>) -> syn::Result<Vec<ViewField>> {
    fields.iter().enumerate().map(|(i, f)| parse_field(f, i)).collect()
}

fn parse_field(field: &syn::Field, position: usize) -> syn::Result<ViewField> {
    let (member, name) = match &field.ident {
        Some(ident) => (Member::Named(ident.clone()), ident.unraw().to_string()),
        None => (Member::Unnamed, position.to_string()),
    };
    let span = field.ident.as_ref().map_or_else(|| field.ty.span(), Ident::span);
    let ty = field.ty.clone();
    let mut spec: Option<FieldSpec> = None;
    let mut column: Option<String> = None;
    let mut json = false;

    let set = |spec: &mut Option<FieldSpec>, value: FieldSpec, span: Span| -> syn::Result<()> {
        if spec.is_some() {
            return Err(syn::Error::new(span, "a field can be only one of `child`, `to_one` or `embed`"));
        }
        *spec = Some(value);
        Ok(())
    };

    for attr in field.attrs.iter().filter(|a| a.path().is_ident("view")) {
        attr.parse_nested_meta(|meta| {
            let span = meta.path.span();
            if meta.path.is_ident("column") {
                column = Some(meta.value()?.parse::<LitStr>()?.value());
            } else if meta.path.is_ident("json") {
                json = true;
            } else if meta.path.is_ident("child") {
                let child = parse_child(&meta, &ty)?;
                set(&mut spec, FieldSpec::Child(Box::new(child)), span)?;
            } else if meta.path.is_ident("to_one") {
                let mut fk = None;
                meta.parse_nested_meta(|inner| {
                    if inner.path.is_ident("fk") {
                        fk = Some(inner.value()?.parse::<LitStr>()?.value());
                    } else {
                        return Err(inner.error("unknown `to_one` attribute, expected `fk`"));
                    }
                    Ok(())
                })?;
                let fk = fk.ok_or_else(|| syn::Error::new(span, "`to_one` needs `fk = \"...\"`"))?;
                let (optional, held) = match generic_argument(&ty, "Option") {
                    Some(inner) => (true, inner.clone()),
                    None => (false, ty.clone()),
                };
                let (form, target) = form_of(&held);
                set(&mut spec, FieldSpec::ToOne { fk, optional, target, form }, span)?;
            } else if meta.path.is_ident("embed") {
                let mut prefix = String::new();
                if meta.input.peek(syn::token::Paren) {
                    meta.parse_nested_meta(|inner| {
                        if inner.path.is_ident("prefix") {
                            prefix = inner.value()?.parse::<LitStr>()?.value();
                        } else {
                            return Err(inner.error("unknown `embed` attribute, expected `prefix`"));
                        }
                        Ok(())
                    })?;
                }
                set(&mut spec, FieldSpec::Embed { prefix, ty: ty.clone() }, span)?;
            } else {
                return Err(
                    meta.error("unknown view attribute, expected `column`, `json`, `child`, `to_one` or `embed`")
                );
            }
            Ok(())
        })?;
    }

    let spec = match (spec, column, json) {
        (Some(_), _, true) => {
            return Err(syn::Error::new(span, "`json` only applies to column fields"));
        }
        (Some(_), Some(_), _) => {
            return Err(syn::Error::new(
                span,
                "`column` only applies to column fields; use `fk` for `child` and `to_one`, or `prefix` for `embed`",
            ));
        }
        (Some(spec), None, false) => spec,
        (None, None, _) if matches!(member, Member::Unnamed) => {
            return Err(syn::Error::new(span, "a tuple field needs `#[view(column = \"...\")]` or `#[view(embed)]`"));
        }
        (None, column, json) => {
            let column = column.unwrap_or_else(|| name.clone());
            if json { FieldSpec::Json { column } } else { FieldSpec::Column { column } }
        }
    };

    Ok(ViewField { vis: field.vis.clone(), member, name, span, ty, spec })
}

fn parse_child(meta: &syn::meta::ParseNestedMeta<'_>, ty: &Type) -> syn::Result<ChildSpec> {
    let span = meta.path.span();
    let mut fk = None;
    let mut order_by = Vec::new();
    let mut through = None;
    let mut target = None;
    let mut index = None;
    let mut map_key = None;
    let mut depth = None;
    let mut cte = false;
    meta.parse_nested_meta(|inner| {
        let string = || -> syn::Result<String> { Ok(inner.value()?.parse::<LitStr>()?.value()) };
        if inner.path.is_ident("fk") {
            fk = Some(string()?);
        } else if inner.path.is_ident("order_by") {
            let lit = inner.value()?.parse::<LitStr>()?;
            order_by = parse_order_by(&lit)?;
        } else if inner.path.is_ident("through") {
            through = Some(string()?);
        } else if inner.path.is_ident("target") {
            target = Some(string()?);
        } else if inner.path.is_ident("index") {
            index = Some(string()?);
        } else if inner.path.is_ident("key") {
            map_key = Some(string()?);
        } else if inner.path.is_ident("depth") {
            let lit = inner.value()?.parse::<syn::LitInt>()?;
            let value: u32 = lit.base10_parse()?;
            if value == 0 {
                return Err(syn::Error::new(lit.span(), "`depth` needs to be at least 1"));
            }
            depth = Some(value);
        } else if inner.path.is_ident("recursive") {
            let lit = inner.value()?.parse::<LitStr>()?;
            if lit.value() != "cte" {
                return Err(syn::Error::new(lit.span(), "expected `recursive = \"cte\"`"));
            }
            cte = true;
        } else {
            return Err(inner.error(
                "unknown `child` attribute, expected `fk`, `order_by`, `through`, `target`, `index`, `key`, \
                 `depth` or `recursive`",
            ));
        }
        Ok(())
    })?;

    let fk = fk.ok_or_else(|| syn::Error::new(span, "`child` needs `fk = \"...\"`"))?;
    let through = match (through, target) {
        (Some(table), Some(target)) => Some((table, target)),
        (None, None) => None,
        _ => return Err(syn::Error::new(span, "`through` and `target` need to be used together")),
    };

    let (element, map_key_type) = match (generic_argument(ty, "Vec"), map_arguments(ty)) {
        (Some(element), _) => {
            if map_key.is_some() {
                return Err(syn::Error::new(ty.span(), "`key` needs a `BTreeMap` or `HashMap` field"));
            }
            (element.clone(), None)
        }
        (None, Some((key, value))) => {
            if map_key.is_none() {
                return Err(syn::Error::new(span, "a map collection needs `key = \"...\"`, the column of the map key"));
            }
            if index.is_some() {
                return Err(syn::Error::new(span, "`index` places the elements of a `Vec`, not of a map"));
            }
            (value.clone(), Some(key.clone()))
        }
        (None, None) => {
            return Err(syn::Error::new(
                ty.span(),
                "a `child` field needs to be a `Vec` of a view, or a `BTreeMap` or `HashMap` of views",
            ));
        }
    };

    let recursion = match (depth, cte) {
        (depth, true) => Some(RecursionSpec::Cte(depth)),
        (Some(depth), false) => Some(RecursionSpec::Depth(depth)),
        (None, false) => None,
    };
    let (form, element) = form_of(&element);
    if form != Form::Owned && map_key_type.is_some() {
        return Err(syn::Error::new(ty.span(), "the values of a map collection need to be owned views"));
    }
    if form == Form::Graph && recursion.is_some() {
        return Err(syn::Error::new(
            span,
            "a graph collection loads the whole graph by itself; it takes no `depth` or `recursive`",
        ));
    }
    Ok(ChildSpec { form, fk, order_by, element, map_key_type, through, index, map_key, recursion })
}

/// The key and value types of `BTreeMap<K, V>` or `HashMap<K, V>`.
fn map_arguments(ty: &Type) -> Option<(&Type, &Type)> {
    let Type::Path(path) = ty else { return None };
    let segment = path.path.segments.last()?;
    if segment.ident != "BTreeMap" && segment.ident != "HashMap" {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &segment.arguments else { return None };
    let mut types = args.args.iter().filter_map(|arg| match arg {
        GenericArgument::Type(ty) => Some(ty),
        _ => None,
    });
    Some((types.next()?, types.next()?))
}

fn parse_order_by(lit: &LitStr) -> syn::Result<Vec<(String, bool)>> {
    let mut result = Vec::new();
    for term in lit.value().split(',') {
        let parts: Vec<&str> = term.split_whitespace().collect();
        match parts.as_slice() {
            [column] => result.push((column.to_string(), false)),
            [column, dir] if dir.eq_ignore_ascii_case("asc") => result.push((column.to_string(), false)),
            [column, dir] if dir.eq_ignore_ascii_case("desc") => result.push((column.to_string(), true)),
            _ => {
                return Err(syn::Error::new(
                    lit.span(),
                    "`order_by` is a comma separated list of columns, each optionally followed by `asc` or `desc`",
                ));
            }
        }
    }
    Ok(result)
}

/// The type argument of `Wrapper<T>`, e.g. `Vec` or `Option`.
fn generic_argument<'a>(ty: &'a Type, wrapper: &str) -> Option<&'a Type> {
    let Type::Path(path) = ty else { return None };
    let segment = path.path.segments.last()?;
    if segment.ident != wrapper {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &segment.arguments else { return None };
    match args.args.first()? {
        GenericArgument::Type(inner) if args.args.len() == 1 => Some(inner),
        _ => None,
    }
}

fn field_shape(field: &ViewField) -> TokenStream2 {
    let name = &field.name;
    let kind = match &field.spec {
        FieldSpec::Column { column } | FieldSpec::Json { column } => {
            quote! { __mabat::__private::FieldKind::Column { column: #column } }
        }
        FieldSpec::Embed { prefix, ty } => quote! {
            __mabat::__private::FieldKind::Embedded {
                column_prefix: #prefix,
                shape: <#ty as __mabat::Embedded>::shape,
            }
        },
        FieldSpec::Child(child) => {
            let ChildSpec { fk, order_by, element, through, index, map_key, recursion, form, .. } = &**child;
            let graph = *form == Form::Graph;
            let order_by = order_by.iter().map(|(column, descending)| {
                quote! { __mabat::__private::OrderBy { column: #column, descending: #descending } }
            });
            let option = |value: &Option<String>| match value {
                Some(value) => quote! { ::core::option::Option::Some(#value) },
                None => quote! { ::core::option::Option::None },
            };
            let through = match through {
                Some((table, target)) => quote! {
                    ::core::option::Option::Some(__mabat::__private::Through { table: #table, target: #target })
                },
                None => quote! { ::core::option::Option::None },
            };
            let index = option(index);
            let map_key = option(map_key);
            let recursion = match recursion {
                Some(RecursionSpec::Depth(depth)) => {
                    quote! { ::core::option::Option::Some(__mabat::__private::Recursion::Depth(#depth)) }
                }
                Some(RecursionSpec::Cte(depth)) => {
                    let depth = match depth {
                        Some(depth) => quote! { ::core::option::Option::Some(#depth) },
                        None => quote! { ::core::option::Option::None },
                    };
                    quote! { ::core::option::Option::Some(__mabat::__private::Recursion::Cte { depth: #depth }) }
                }
                None => quote! { ::core::option::Option::None },
            };
            quote! {
                __mabat::__private::FieldKind::Child(__mabat::__private::Child {
                    fk: #fk,
                    order_by: &[#(#order_by),*],
                    shape: <#element as __mabat::View>::shape,
                    through: #through,
                    index: #index,
                    map_key: #map_key,
                    recursion: #recursion,
                    graph: #graph,
                })
            }
        }
        FieldSpec::ToOne { fk, optional, target, form } => {
            let graph = *form == Form::Graph;
            quote! {
                __mabat::__private::FieldKind::ToOne {
                    fk: #fk,
                    optional: #optional,
                    shape: <#target as __mabat::View>::shape,
                    graph: #graph,
                }
            }
        }
    };
    quote! { __mabat::__private::Field { name: #name, kind: #kind } }
}

/// Where the fields being generated are, which decides how their aliases and field indices
/// are written.
#[derive(Clone, Copy)]
enum Scope<'a> {
    /// The fields of a view or of a variant table: aliases are the field names, and the
    /// field index of a field is its own.
    Row,
    /// Fields in the row of the containing view, under the runtime `prefix` followed by the
    /// given text: embedded structs and variants stored in columns. Field indices are the
    /// `field_index` argument.
    Prefixed(&'a str),
}

impl Scope<'_> {
    fn alias(&self, name: &str) -> TokenStream2 {
        match self {
            Scope::Row => quote! { #name },
            Scope::Prefixed(text) => {
                let suffix = format!("{text}{name}");
                quote! { &::std::format!("{}{}", prefix, #suffix) }
            }
        }
    }

    fn embed_prefix(&self, name: &str) -> TokenStream2 {
        match self {
            Scope::Row => {
                let prefix = format!("{name}.");
                quote! { #prefix }
            }
            Scope::Prefixed(text) => {
                let suffix = format!("{text}{name}.");
                quote! { &::std::format!("{}{}", prefix, #suffix) }
            }
        }
    }

    fn field_index(&self, index: usize) -> TokenStream2 {
        match self {
            Scope::Row => quote! { #index },
            Scope::Prefixed(_) => quote! { field_index },
        }
    }
}

/// The expression that decodes a field from `row` and `node`.
fn decode_value(field: &ViewField, index: usize, scope: Scope<'_>) -> TokenStream2 {
    let ty = &field.ty;
    let name = &field.name;
    let alias = scope.alias(name);
    match &field.spec {
        FieldSpec::Column { .. } => match generic_argument(ty, "Option") {
            Some(inner) => quote! { __mabat::__private::optional_column::<#inner, __Backend>(row, node, #alias)? },
            None => quote! { __mabat::__private::column::<#ty, __Backend>(row, node, #alias)? },
        },
        FieldSpec::Json { .. } => match generic_argument(ty, "Option") {
            Some(inner) => quote! {
                __mabat::__private::optional_column::<__mabat::__private::Json<#inner>, __Backend>(row, node, #alias)?
                    .map(|json| json.0)
            },
            None => {
                quote! { __mabat::__private::column::<__mabat::__private::Json<#ty>, __Backend>(row, node, #alias)?.0 }
            }
        },
        FieldSpec::Embed { ty, .. } => {
            let prefix = scope.embed_prefix(name);
            let field_index = scope.field_index(index);
            quote! { <#ty as __mabat::EmbeddedDecoder<__Backend>>::decode_embedded(row, node, #prefix, #field_index)? }
        }
        FieldSpec::Child(child) => {
            let element = &child.element;
            match (&child.map_key_type, child.form) {
                (Some(key), _) => quote! { __mabat::__private::map::<#key, #element, #ty, _>(row, node, #index)? },
                (None, Form::Owned) => quote! { __mabat::__private::children::<#element, _>(row, node, #index)? },
                (None, Form::Shared) => {
                    quote! { __mabat::__private::shared_children::<#element, _>(row, node, #index)? }
                }
                (None, Form::Graph) => quote! { __mabat::__private::references::<#element, _>(row, node, #index)? },
            }
        }
        FieldSpec::ToOne { optional, target, form, .. } => {
            let ref_alias = format!("$ref.{name}");
            let helper = match (form, optional) {
                (Form::Owned, true) => quote! { to_one::<#target, _>(row, node, #index, #ref_alias) },
                (Form::Owned, false) => quote! { to_one_required::<#target, _>(row, node, #index, #ref_alias) },
                (Form::Shared, true) => quote! { shared_to_one::<#target, _>(row, node, #index, #ref_alias) },
                (Form::Shared, false) => quote! { shared_to_one_required::<#target, _>(row, node, #index, #ref_alias) },
                (Form::Graph, true) => quote! { reference::<#target, _>(row, node, #ref_alias) },
                (Form::Graph, false) => quote! { reference_required::<#target, _>(row, node, #ref_alias) },
            };
            quote! { __mabat::__private::#helper? }
        }
    }
}

/// The statement that describes the Rust type of a field to `description`.
fn describe_value(field: &ViewField, index: usize, scope: Scope<'_>) -> TokenStream2 {
    let ty = &field.ty;
    let name = &field.name;
    let alias = scope.alias(name);
    let optional = generic_argument(ty, "Option");
    let is_optional = optional.is_some();
    match &field.spec {
        FieldSpec::Column { .. } => quote! { description.column::<#ty>(#alias, #is_optional); },
        FieldSpec::Json { .. } => {
            let inner = optional.unwrap_or(ty);
            quote! { description.column::<__mabat::__private::Json<#inner>>(#alias, #is_optional); }
        }
        FieldSpec::Embed { ty, .. } => {
            let prefix = scope.embed_prefix(name);
            let field_index = scope.field_index(index);
            quote! { <#ty as __mabat::EmbeddedDecoder<__Backend>>::describe_embedded(description, #prefix, #field_index); }
        }
        FieldSpec::Child(child) => {
            let element = &child.element;
            match &child.map_key_type {
                None => quote! { description.view::<#element>(#index); },
                Some(key) => quote! { description.map::<#key, #element>(#index); },
            }
        }
        FieldSpec::ToOne { target, .. } => quote! { description.view::<#target>(#index); },
    }
}

/// The expression that builds a struct or variant from its decoded fields.
fn construct(path: TokenStream2, style: Style, fields: &[ViewField], scope: Scope<'_>) -> TokenStream2 {
    let values = fields.iter().enumerate().map(|(index, field)| {
        let value = decode_value(field, index, scope);
        match &field.member {
            Member::Named(ident) => quote! { #ident: #value },
            Member::Unnamed => value,
        }
    });
    match style {
        Style::Unit => path,
        Style::Named => quote! { #path { #(#values),* } },
        Style::Tuple => quote! { #path ( #(#values),* ) },
    }
}

/// The databases decoders are generated for: the name in `#[view(databases = "...")]`, the
/// facade's macro that keeps the decoder when the database's feature is enabled, and the
/// database type.
const BACKENDS: [(&str, &str, &str); 3] =
    [("postgres", "__if_postgres", "Postgres"), ("mysql", "__if_mysql", "MySql"), ("sqlite", "__if_sqlite", "Sqlite")];

/// The databases a view is decoded on, by their position in [`BACKENDS`].
#[derive(Clone, Copy)]
struct Databases([bool; 3]);

impl Databases {
    const ALL: Databases = Databases([true; 3]);

    fn parse(lit: &LitStr) -> syn::Result<Databases> {
        let mut databases = [false; 3];
        for name in lit.value().split(',').map(str::trim) {
            let position = BACKENDS.iter().position(|(n, _, _)| *n == name).ok_or_else(|| {
                syn::Error::new(
                    lit.span(),
                    format!("unknown database `{name}`, expected `postgres`, `mysql` or `sqlite`"),
                )
            })?;
            databases[position] = true;
        }
        Ok(Databases(databases))
    }
}

/// An item for each database, kept by the facade only for the enabled ones. `make` gets the
/// path of the database type; generated bodies refer to it as `__Backend`.
fn per_backend(databases: Databases, make: impl Fn(&TokenStream2) -> TokenStream2) -> TokenStream2 {
    let items = BACKENDS.iter().zip(databases.0).filter(|(_, on)| *on).map(|((_, gate, ty), _)| {
        let gate = Ident::new(gate, Span::call_site());
        let ty = Ident::new(ty, Span::call_site());
        let item = make(&quote! { __mabat::__private::#ty });
        quote! { __mabat::#gate! { #item } }
    });
    quote! { #(#items)* }
}

fn expand_view(
    ident: &Ident,
    table: &str,
    key: &str,
    fields: &[ViewField],
    databases: Databases,
) -> syn::Result<TokenStream2> {
    let view_name = ident.to_string();
    let count = fields.len();
    let shapes = fields.iter().map(field_shape);
    let value = construct(quote! { Self }, Style::Named, fields, Scope::Row);
    let describers = fields.iter().enumerate().map(|(index, field)| describe_value(field, index, Scope::Row));
    let visits = fields.iter().enumerate().filter_map(|(index, field)| {
        let (target, entity) = match &field.spec {
            FieldSpec::Child(child) => (&child.element, child.form == Form::Graph),
            FieldSpec::ToOne { target, form, .. } => (target, *form == Form::Graph),
            _ => return None,
        };
        Some(quote! { __mabat::__private::graph_visit::<#target, _>(node, #index, graph, #entity)?; })
    });
    let navigation = navigation(ident, fields);

    let describers: Vec<TokenStream2> = describers.collect();
    let visits: Vec<TokenStream2> = visits.collect();
    let decoders = per_backend(databases, |backend| {
        quote! {
            impl __mabat::ViewDecoder<#backend> for #ident {
                fn decode(
                    row: &<#backend as __mabat::__private::Database>::Row,
                    node: &__mabat::Node<#backend>,
                ) -> ::core::result::Result<Self, __mabat::Error> {
                    #[allow(dead_code)]
                    type __Backend = #backend;
                    ::core::result::Result::Ok(#value)
                }

                #[allow(unused_variables)]
                fn describe(description: &mut __mabat::__private::Description<#backend>) {
                    #[allow(dead_code)]
                    type __Backend = #backend;
                    #(#describers)*
                }

                #[allow(unused_variables)]
                fn decode_graph(
                    node: &__mabat::Node<#backend>,
                    graph: &mut __mabat::__private::GraphBuilder,
                    entity: bool,
                ) -> ::core::result::Result<(), __mabat::Error> {
                    if entity {
                        __mabat::__private::graph_store::<Self, _>(node, graph)?;
                    }
                    #(#visits)*
                    ::core::result::Result::Ok(())
                }
            }
        }
    });

    Ok(quote! {
        #navigation

        impl __mabat::View for #ident {
            fn shape() -> &'static __mabat::__private::ViewShape {
                static FIELDS: [__mabat::__private::Field; #count] = [#(#shapes),*];
                static SHAPE: __mabat::__private::ViewShape = __mabat::__private::ViewShape {
                    name: #view_name,
                    table: #table,
                    key_column: #key,
                    fields: &FIELDS,
                };
                &SHAPE
            }
        }

        #decoders
    })
}

/// Methods that follow the references of a view's fields into a graph, named after the
/// fields: `task.parent(&graph)` for `parent: Option<Ref<Task>>`.
fn navigation(owner: &Ident, fields: &[ViewField]) -> TokenStream2 {
    let methods: Vec<TokenStream2> = fields
        .iter()
        .filter_map(|field| {
            let Member::Named(ident) = &field.member else { return None };
            let vis = &field.vis;
            let doc = format!("Follow `{}` in the graph.", field.name);
            let method = match &field.spec {
                FieldSpec::ToOne { form: Form::Graph, optional: true, target, .. } => quote! {
                    #vis fn #ident<'g, R>(&'g self, graph: &'g __mabat::Graph<R>) -> ::core::option::Option<&'g #target> {
                        self.#ident.map(|r| graph.get(r))
                    }
                },
                FieldSpec::ToOne { form: Form::Graph, optional: false, target, .. } => quote! {
                    #vis fn #ident<'g, R>(&'g self, graph: &'g __mabat::Graph<R>) -> &'g #target {
                        graph.get(self.#ident)
                    }
                },
                FieldSpec::Child(child) if child.form == Form::Graph => {
                    let target = &child.element;
                    quote! {
                        #vis fn #ident<'g, R>(
                            &'g self,
                            graph: &'g __mabat::Graph<R>,
                        ) -> impl ::core::iter::Iterator<Item = &'g #target> + 'g {
                            self.#ident.iter().map(move |r| graph.get(*r))
                        }
                    }
                }
                _ => return None,
            };
            Some(quote! { #[doc = #doc] #method })
        })
        .collect();
    if methods.is_empty() {
        return TokenStream2::new();
    }
    quote! {
        impl #owner {
            #(#methods)*
        }
    }
}

fn expand_embedded(ident: &Ident, fields: &[ViewField], databases: Databases) -> syn::Result<TokenStream2> {
    for field in fields {
        if matches!(field.spec, FieldSpec::Child(_) | FieldSpec::ToOne { .. }) {
            return Err(syn::Error::new(
                field.span,
                "an embedded struct can only contain columns and other embedded structs",
            ));
        }
    }

    let view_name = ident.to_string();
    let count = fields.len();
    let shapes = fields.iter().map(field_shape);
    let scope = Scope::Prefixed("");
    let value = construct(quote! { Self }, Style::Named, fields, scope);
    let describers = fields.iter().enumerate().map(|(index, field)| describe_value(field, index, scope));

    let describers: Vec<TokenStream2> = describers.collect();
    let decoders = per_backend(databases, |backend| {
        quote! {
            impl __mabat::EmbeddedDecoder<#backend> for #ident {
                #[allow(unused_variables)]
                fn decode_embedded(
                    row: &<#backend as __mabat::__private::Database>::Row,
                    node: &__mabat::Node<#backend>,
                    prefix: &str,
                    field_index: usize,
                ) -> ::core::result::Result<Self, __mabat::Error> {
                    #[allow(dead_code)]
                    type __Backend = #backend;
                    ::core::result::Result::Ok(#value)
                }

                #[allow(unused_variables)]
                fn describe_embedded(
                    description: &mut __mabat::__private::Description<#backend>,
                    prefix: &str,
                    field_index: usize,
                ) {
                    #[allow(dead_code)]
                    type __Backend = #backend;
                    #(#describers)*
                }
            }
        }
    });

    Ok(quote! {
        impl __mabat::Embedded for #ident {
            fn shape() -> &'static __mabat::__private::EmbeddedShape {
                static FIELDS: [__mabat::__private::Field; #count] = [#(#shapes),*];
                static SHAPE: __mabat::__private::EmbeddedShape = __mabat::__private::EmbeddedShape {
                    name: #view_name,
                    kind: __mabat::__private::EmbeddedKind::Product { fields: &FIELDS },
                };
                &SHAPE
            }
        }

        #decoders
    })
}

fn expand_sum(
    ident: &Ident,
    tag: &str,
    strategy: Strategy,
    lenient: bool,
    variants: &[VariantSpec],
    databases: Databases,
) -> syn::Result<TokenStream2> {
    let enum_name = ident.to_string();

    // Statics of the shape: the fields of each variant, and the view shape of each variant table
    let mut statics = Vec::new();
    let mut variant_shapes = Vec::new();
    for (i, variant) in variants.iter().enumerate() {
        let name = variant.ident.unraw().to_string();
        let tag_value = &variant.tag_value;
        let fields_static = format_ident!("V{i}_FIELDS");
        let count = variant.fields.len();
        let shapes = variant.fields.iter().map(field_shape);
        let data = match (&variant.table, variant.style) {
            (_, Style::Unit) => quote! { __mabat::__private::VariantData::Unit },
            (None, _) => {
                statics.push(quote! { static #fields_static: [__mabat::__private::Field; #count] = [#(#shapes),*]; });
                quote! { __mabat::__private::VariantData::Columns { fields: &#fields_static } }
            }
            (Some((table, key)), _) => {
                let shape_static = format_ident!("V{i}_SHAPE");
                let shape_fn = format_ident!("v{i}_shape");
                let view_name = format!("{enum_name}::{name}");
                statics.push(quote! {
                    static #fields_static: [__mabat::__private::Field; #count] = [#(#shapes),*];
                    static #shape_static: __mabat::__private::ViewShape = __mabat::__private::ViewShape {
                        name: #view_name,
                        table: #table,
                        key_column: #key,
                        fields: &#fields_static,
                    };
                    fn #shape_fn() -> &'static __mabat::__private::ViewShape {
                        &#shape_static
                    }
                });
                quote! { __mabat::__private::VariantData::Table { shape: #shape_fn } }
            }
        };
        variant_shapes.push(quote! {
            __mabat::__private::Variant { name: #name, tag_value: #tag_value, data: #data }
        });
    }
    let variant_count = variants.len();
    let strategy_tokens = match strategy {
        Strategy::Tag => quote! { __mabat::__private::SumStrategy::Tag },
        Strategy::TablePerVariant => quote! { __mabat::__private::SumStrategy::TablePerVariant },
    };

    // Decoding: a match on the tag
    let arms = variants.iter().map(|variant| {
        let variant_ident = &variant.ident;
        let name = variant.ident.unraw().to_string();
        let tag_value = &variant.tag_value;
        let path = quote! { Self::#variant_ident };
        match &variant.table {
            None => {
                let text = format!("{name}.");
                let value = construct(path, variant.style, &variant.fields, Scope::Prefixed(&text));
                quote! {
                    #tag_value => {
                        __mabat::__private::strict(row, node, prefix, #name, #tag_value)?;
                        ::core::result::Result::Ok(#value)
                    }
                }
            }
            Some(_) => {
                let value = construct(path, variant.style, &variant.fields, Scope::Row);
                quote! {
                    #tag_value => {
                        let (row, node) = __mabat::__private::variant(row, node, field_index, #name)?;
                        ::core::result::Result::Ok(#value)
                    }
                }
            }
        }
    });
    let tag_values = variants.iter().map(|v| &v.tag_value);

    let describers = variants.iter().map(|variant| {
        let name = variant.ident.unraw().to_string();
        match &variant.table {
            None => {
                let text = format!("{name}.");
                let fields = variant
                    .fields
                    .iter()
                    .enumerate()
                    .map(|(i, field)| describe_value(field, i, Scope::Prefixed(&text)));
                quote! { #(#fields)* }
            }
            Some(_) => {
                let fields = variant.fields.iter().enumerate().map(|(i, field)| describe_value(field, i, Scope::Row));
                quote! {
                    description.variant_table(field_index, #name, |description: &mut __mabat::__private::Description<__Backend>| {
                        #(#fields)*
                    });
                }
            }
        }
    });

    let arms: Vec<TokenStream2> = arms.collect();
    let tag_values: Vec<&String> = tag_values.collect();
    let describers: Vec<TokenStream2> = describers.collect();
    let decoders = per_backend(databases, |backend| {
        quote! {
            impl __mabat::EmbeddedDecoder<#backend> for #ident {
                #[allow(unused_variables)]
                fn decode_embedded(
                    row: &<#backend as __mabat::__private::Database>::Row,
                    node: &__mabat::Node<#backend>,
                    prefix: &str,
                    field_index: usize,
                ) -> ::core::result::Result<Self, __mabat::Error> {
                    #[allow(dead_code)]
                    type __Backend = #backend;
                    let tag = __mabat::__private::tag(row, node, prefix)?;
                    match tag.as_str() {
                        #(#arms)*
                        other => ::core::result::Result::Err(
                            __mabat::__private::unknown_tag(node, prefix, other, &[#(#tag_values),*]),
                        ),
                    }
                }

                #[allow(unused_variables)]
                fn describe_embedded(
                    description: &mut __mabat::__private::Description<#backend>,
                    prefix: &str,
                    field_index: usize,
                ) {
                    #[allow(dead_code)]
                    type __Backend = #backend;
                    description.column::<::std::string::String>(::std::format!("{}$tag", prefix), false);
                    #(#describers)*
                }
            }
        }
    });

    Ok(quote! {
        impl __mabat::Embedded for #ident {
            fn shape() -> &'static __mabat::__private::EmbeddedShape {
                #(#statics)*
                static VARIANTS: [__mabat::__private::Variant; #variant_count] = [#(#variant_shapes),*];
                static SHAPE: __mabat::__private::EmbeddedShape = __mabat::__private::EmbeddedShape {
                    name: #enum_name,
                    kind: __mabat::__private::EmbeddedKind::Sum(__mabat::__private::SumShape {
                        tag_column: #tag,
                        strategy: #strategy_tokens,
                        lenient: #lenient,
                        variants: &VARIANTS,
                    }),
                };
                &SHAPE
            }
        }

        #decoders
    })
}
