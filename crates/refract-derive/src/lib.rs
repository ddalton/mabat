//! `#[derive(View)]` for Refract.
//!
//! Use it through the `refract` crate, which documents the attributes.

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::quote;
use syn::ext::IdentExt;
use syn::spanned::Spanned;
use syn::{Data, DeriveInput, Fields, GenericArgument, Ident, LitStr, PathArguments, Type};

/// Derive a Refract view.
///
/// On the struct, either `#[view(table = "...", key = "...")]` for a view loaded from a
/// table (`key` defaults to `id`), or `#[view(embedded)]` for a struct stored in columns of
/// the table of the view that contains it.
///
/// On fields:
/// - `#[view(column = "...")]`: the column, when it differs from the field name
/// - `#[view(child(fk = "...", order_by = "a, b desc"))]`: a `Vec` loaded by a child query
/// - `#[view(to_one(fk = "..."))]`: a reference to another view, `Option` if the foreign
///   key is nullable
/// - `#[view(embed)]` or `#[view(embed(prefix = "..."))]`: an embedded struct, with an
///   optional common column prefix
#[proc_macro_derive(View, attributes(view))]
pub fn derive_view(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as DeriveInput);
    expand(input).unwrap_or_else(syn::Error::into_compile_error).into()
}

enum Target {
    View { table: String, key: String },
    Embedded,
}

enum FieldSpec {
    Column { column: String },
    Embed { prefix: String, ty: Type },
    Child { fk: String, order_by: Vec<(String, bool)>, element: Type },
    ToOne { fk: String, optional: bool, target: Type },
}

struct ViewField {
    ident: Ident,
    name: String,
    ty: Type,
    spec: FieldSpec,
}

fn expand(input: DeriveInput) -> syn::Result<TokenStream2> {
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new(input.generics.span(), "a view cannot have generic parameters"));
    }
    let target = parse_target(&input)?;
    let fields = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(named) => named.named.iter().map(parse_field).collect::<syn::Result<Vec<_>>>()?,
            _ => return Err(syn::Error::new(input.ident.span(), "a view needs to be a struct with named fields")),
        },
        _ => {
            return Err(syn::Error::new(
                input.ident.span(),
                "only structs can derive View; enums are planned for a later milestone",
            ));
        }
    };

    match target {
        Target::View { table, key } => expand_view(&input.ident, &table, &key, &fields),
        Target::Embedded => expand_embedded(&input.ident, &fields),
    }
}

fn parse_target(input: &DeriveInput) -> syn::Result<Target> {
    let mut table = None;
    let mut key = None;
    let mut embedded = false;
    for attr in input.attrs.iter().filter(|a| a.path().is_ident("view")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("table") {
                table = Some(meta.value()?.parse::<LitStr>()?.value());
            } else if meta.path.is_ident("key") {
                key = Some(meta.value()?.parse::<LitStr>()?.value());
            } else if meta.path.is_ident("embedded") {
                embedded = true;
            } else {
                return Err(meta.error("unknown view attribute, expected `table`, `key` or `embedded`"));
            }
            Ok(())
        })?;
    }

    match (embedded, table) {
        (true, None) if key.is_none() => Ok(Target::Embedded),
        (true, _) => Err(syn::Error::new(input.ident.span(), "an embedded struct has no `table` or `key`")),
        (false, Some(table)) => Ok(Target::View { table, key: key.unwrap_or_else(|| "id".to_string()) }),
        (false, None) => Err(syn::Error::new(
            input.ident.span(),
            "missing `#[view(table = \"...\")]`, or `#[view(embedded)]` for an embedded struct",
        )),
    }
}

fn parse_field(field: &syn::Field) -> syn::Result<ViewField> {
    let ident = field.ident.clone().expect("named field");
    let name = ident.unraw().to_string();
    let ty = field.ty.clone();
    let mut spec: Option<FieldSpec> = None;
    let mut column: Option<String> = None;

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
            } else if meta.path.is_ident("child") {
                let mut fk = None;
                let mut order_by = Vec::new();
                meta.parse_nested_meta(|inner| {
                    if inner.path.is_ident("fk") {
                        fk = Some(inner.value()?.parse::<LitStr>()?.value());
                    } else if inner.path.is_ident("order_by") {
                        let lit = inner.value()?.parse::<LitStr>()?;
                        order_by = parse_order_by(&lit)?;
                    } else {
                        return Err(inner.error("unknown `child` attribute, expected `fk` or `order_by`"));
                    }
                    Ok(())
                })?;
                let fk = fk.ok_or_else(|| syn::Error::new(span, "`child` needs `fk = \"...\"`"))?;
                let element = generic_argument(&ty, "Vec")
                    .ok_or_else(|| syn::Error::new(ty.span(), "a `child` field needs to be a `Vec` of a view"))?;
                set(&mut spec, FieldSpec::Child { fk, order_by, element: element.clone() }, span)?;
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
                let (optional, target) = match generic_argument(&ty, "Option") {
                    Some(inner) => (true, inner.clone()),
                    None => (false, ty.clone()),
                };
                set(&mut spec, FieldSpec::ToOne { fk, optional, target }, span)?;
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
                return Err(meta.error("unknown view attribute, expected `column`, `child`, `to_one` or `embed`"));
            }
            Ok(())
        })?;
    }

    let spec = match (spec, column) {
        (None, column) => FieldSpec::Column { column: column.unwrap_or_else(|| name.clone()) },
        (Some(_), Some(_)) => {
            return Err(syn::Error::new(
                ident.span(),
                "`column` only applies to column fields; use `fk` for `child` and `to_one`, or `prefix` for `embed`",
            ));
        }
        (Some(spec), None) => spec,
    };

    Ok(ViewField { ident, name, ty, spec })
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
        FieldSpec::Column { column } => quote! { ::refract::__private::FieldKind::Column { column: #column } },
        FieldSpec::Embed { prefix, ty } => quote! {
            ::refract::__private::FieldKind::Embedded {
                column_prefix: #prefix,
                shape: <#ty as ::refract::Embedded>::shape,
            }
        },
        FieldSpec::Child { fk, order_by, element } => {
            let order_by = order_by.iter().map(|(column, descending)| {
                quote! { ::refract::__private::OrderBy { column: #column, descending: #descending } }
            });
            quote! {
                ::refract::__private::FieldKind::Child {
                    fk: #fk,
                    order_by: &[#(#order_by),*],
                    shape: <#element as ::refract::View>::shape,
                }
            }
        }
        FieldSpec::ToOne { fk, optional, target } => quote! {
            ::refract::__private::FieldKind::ToOne {
                fk: #fk,
                optional: #optional,
                shape: <#target as ::refract::View>::shape,
            }
        },
    };
    quote! { ::refract::__private::Field { name: #name, kind: #kind } }
}

fn expand_view(ident: &Ident, table: &str, key: &str, fields: &[ViewField]) -> syn::Result<TokenStream2> {
    let view_name = ident.to_string();
    let count = fields.len();
    let shapes = fields.iter().map(field_shape);

    let decoders = fields.iter().enumerate().map(|(index, field)| {
        let ident = &field.ident;
        let ty = &field.ty;
        let name = &field.name;
        let value = match &field.spec {
            FieldSpec::Column { .. } => match generic_argument(ty, "Option") {
                Some(inner) => quote! { ::refract::__private::optional_column::<#inner>(row, node, #name)? },
                None => quote! { ::refract::__private::column::<#ty>(row, node, #name)? },
            },
            FieldSpec::Embed { ty, .. } => {
                let prefix = format!("{name}.");
                quote! { <#ty as ::refract::Embedded>::decode_embedded(row, node, #prefix)? }
            }
            FieldSpec::Child { element, .. } => {
                quote! { ::refract::__private::children::<#element>(row, node, #index)? }
            }
            FieldSpec::ToOne { optional, target, .. } => {
                let ref_alias = format!("$ref.{name}");
                if *optional {
                    quote! { ::refract::__private::to_one::<#target>(row, node, #index, #ref_alias)? }
                } else {
                    quote! { ::refract::__private::to_one_required::<#target>(row, node, #index, #ref_alias)? }
                }
            }
        };
        quote! { #ident: #value }
    });

    let describers = fields.iter().enumerate().map(|(index, field)| {
        let ty = &field.ty;
        let name = &field.name;
        match &field.spec {
            FieldSpec::Column { .. } => {
                let optional = generic_argument(ty, "Option").is_some();
                quote! { description.column::<#ty>(#name, #optional); }
            }
            FieldSpec::Embed { ty, .. } => {
                let prefix = format!("{name}.");
                quote! { <#ty as ::refract::Embedded>::describe_embedded(description, #prefix); }
            }
            FieldSpec::Child { element: target, .. } | FieldSpec::ToOne { target, .. } => {
                quote! { description.view::<#target>(#index); }
            }
        }
    });

    Ok(quote! {
        impl ::refract::View for #ident {
            fn shape() -> &'static ::refract::__private::ViewShape {
                static FIELDS: [::refract::__private::Field; #count] = [#(#shapes),*];
                static SHAPE: ::refract::__private::ViewShape = ::refract::__private::ViewShape {
                    name: #view_name,
                    table: #table,
                    key_column: #key,
                    fields: &FIELDS,
                };
                &SHAPE
            }

            fn decode(
                row: &::refract::__private::PgRow,
                node: &::refract::Node,
            ) -> ::core::result::Result<Self, ::refract::Error> {
                ::core::result::Result::Ok(Self { #(#decoders),* })
            }

            #[allow(unused_variables)]
            fn describe(description: &mut ::refract::__private::Description) {
                #(#describers)*
            }
        }
    })
}

fn expand_embedded(ident: &Ident, fields: &[ViewField]) -> syn::Result<TokenStream2> {
    for field in fields {
        if matches!(field.spec, FieldSpec::Child { .. } | FieldSpec::ToOne { .. }) {
            return Err(syn::Error::new(
                field.ident.span(),
                "an embedded struct can only contain columns and other embedded structs",
            ));
        }
    }

    let view_name = ident.to_string();
    let count = fields.len();
    let shapes = fields.iter().map(field_shape);
    let decoders = fields.iter().map(|field| {
        let ident = &field.ident;
        let ty = &field.ty;
        let name = &field.name;
        let value = match &field.spec {
            FieldSpec::Column { .. } => match generic_argument(ty, "Option") {
                Some(inner) => quote! {
                    ::refract::__private::optional_column::<#inner>(row, node, &::std::format!("{}{}", prefix, #name))?
                },
                None => quote! {
                    ::refract::__private::column::<#ty>(row, node, &::std::format!("{}{}", prefix, #name))?
                },
            },
            FieldSpec::Embed { ty, .. } => quote! {
                <#ty as ::refract::Embedded>::decode_embedded(row, node, &::std::format!("{}{}.", prefix, #name))?
            },
            FieldSpec::Child { .. } | FieldSpec::ToOne { .. } => unreachable!("rejected above"),
        };
        quote! { #ident: #value }
    });
    let describers = fields.iter().map(|field| {
        let ty = &field.ty;
        let name = &field.name;
        match &field.spec {
            FieldSpec::Column { .. } => {
                let optional = generic_argument(ty, "Option").is_some();
                quote! { description.column::<#ty>(::std::format!("{}{}", prefix, #name), #optional); }
            }
            FieldSpec::Embed { ty, .. } => quote! {
                <#ty as ::refract::Embedded>::describe_embedded(description, &::std::format!("{}{}.", prefix, #name));
            },
            FieldSpec::Child { .. } | FieldSpec::ToOne { .. } => unreachable!("rejected above"),
        }
    });

    Ok(quote! {
        impl ::refract::Embedded for #ident {
            fn shape() -> &'static ::refract::__private::EmbeddedShape {
                static FIELDS: [::refract::__private::Field; #count] = [#(#shapes),*];
                static SHAPE: ::refract::__private::EmbeddedShape = ::refract::__private::EmbeddedShape {
                    name: #view_name,
                    fields: &FIELDS,
                };
                &SHAPE
            }

            fn decode_embedded(
                row: &::refract::__private::PgRow,
                node: &::refract::Node,
                prefix: &str,
            ) -> ::core::result::Result<Self, ::refract::Error> {
                ::core::result::Result::Ok(Self { #(#decoders),* })
            }

            #[allow(unused_variables)]
            fn describe_embedded(description: &mut ::refract::__private::Description, prefix: &str) {
                #(#describers)*
            }
        }
    })
}
