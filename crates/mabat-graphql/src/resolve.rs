//! Resolving the fields of the generated types from the JSON of a load.
//!
//! A root field loads its views as JSON with the selection of the query. The objects below
//! it are those JSON objects, and each field resolver reads its field from its parent's.

use async_graphql::SelectionField;
use async_graphql::dynamic::{FieldFuture, FieldValue, ResolverContext};
use async_graphql::{Name, Value};
use mabat::shape::{EmbeddedKind, FieldKind, ViewShape};
use mabat::{Nested, Selection};
use serde_json::Value as Json;

/// The field of a JSON object that names the variant of an enum.
pub(crate) const TYPENAME: &str = "__typename";

/// The JSON object of the parent of a field.
fn parent<'a>(ctx: &'a ResolverContext<'a>) -> async_graphql::Result<&'a Json> {
    ctx.parent_value.try_downcast_ref::<Json>()
}

/// The value of the field `name` of the parent, `None` if it is missing or null.
fn field<'a>(ctx: &'a ResolverContext<'a>, name: &str) -> async_graphql::Result<Option<&'a Json>> {
    Ok(parent(ctx)?.get(name).filter(|value| !value.is_null()))
}

/// How a field of a generated type is resolved.
#[derive(Debug, Clone)]
pub(crate) enum Resolve {
    /// A scalar, or a list of scalars.
    Scalar,
    /// An object: a view or an embedded struct.
    Object,
    /// A list of objects.
    List,
    /// A map, as a list of `{ key, value }` objects.
    Entries,
    /// An enum with data, as the object of its variant: the union member named after the enum
    /// and the variant.
    Union { prefix: String },
    /// An enum without data, as a GraphQL enum value.
    Enum,
    /// The `_variant` field of a variant object: the variant's name.
    Variant,
}

impl Resolve {
    /// The resolver of the field `name`.
    pub(crate) fn resolver(
        self,
        name: String,
    ) -> impl for<'a> Fn(ResolverContext<'a>) -> FieldFuture<'a> + Send + Sync {
        move |ctx| {
            let how = self.clone();
            let name = name.clone();
            FieldFuture::new(async move {
                let value = match how {
                    Resolve::Variant => parent(&ctx)?.get(TYPENAME),
                    _ => field(&ctx, &name)?,
                };
                let Some(value) = value else { return Ok(None) };
                Ok(Some(match how {
                    Resolve::Scalar => FieldValue::value(Value::from_json(value.clone())?),
                    Resolve::Object => FieldValue::owned_any(value.clone()),
                    Resolve::List => FieldValue::list(list(value)?.iter().map(|v| FieldValue::owned_any(v.clone()))),
                    Resolve::Entries => {
                        let map = value.as_object().ok_or("a map is not a JSON object")?;
                        FieldValue::list(map.iter().map(|(key, value)| {
                            FieldValue::owned_any(serde_json::json!({ "key": key, "value": value }))
                        }))
                    }
                    Resolve::Union { prefix } => {
                        let variant = typename(value)?;
                        FieldValue::owned_any(value.clone()).with_type(format!("{prefix}{variant}"))
                    }
                    Resolve::Enum => FieldValue::value(Value::Enum(Name::new(typename(value)?))),
                    Resolve::Variant => FieldValue::value(value.as_str().ok_or("a variant name is not a string")?),
                }))
            })
        }
    }
}

fn list(value: &Json) -> async_graphql::Result<&Vec<Json>> {
    value.as_array().ok_or_else(|| "a collection is not a JSON array".into())
}

fn typename(value: &Json) -> async_graphql::Result<&str> {
    value.get(TYPENAME).and_then(Json::as_str).ok_or_else(|| "an enum has no __typename".into())
}

/// The arguments of the nested collections of a query: the name of their query, their
/// arguments as written, and as a [`Nested`].
pub(crate) type NestedArguments = Vec<(String, Vec<(Name, Value)>, Nested)>;

/// The selection of a field of type `shape` in a query at `path`: the fields of its
/// selection set, with the views of collections and references selected by their own
/// selection sets. Embedded structs and enums are loaded whole, and the entries of a map
/// select the fields of its `value`. The arguments of nested collections are added to
/// `nested`.
pub(crate) fn selection(
    shape: &'static ViewShape,
    field: &SelectionField<'_>,
    path: &str,
    nested: &mut NestedArguments,
) -> async_graphql::Result<Selection> {
    let mut selection = Selection::new();
    for sub in field.selection_set() {
        let Some(view_field) = shape.fields.iter().find(|f| f.name == sub.name()) else { continue };
        let sub_path =
            if path.is_empty() { view_field.name.to_string() } else { format!("{path}.{}", view_field.name) };
        selection = match &view_field.kind {
            FieldKind::Column { .. } | FieldKind::Embedded { .. } => selection.field(view_field.name),
            FieldKind::Child(child) if child.map_key.is_some() => {
                let mut fields = Selection::new();
                for entry in sub.selection_set().filter(|s| s.name() == "value") {
                    fields.merge(self::selection((child.shape)(), &entry, &sub_path, nested)?);
                }
                selection.nested(view_field.name, fields)
            }
            FieldKind::Child(child) => {
                let target = (child.shape)();
                let arguments = sub.arguments()?;
                match nested.iter().find(|(p, _, _)| *p == sub_path) {
                    Some((_, existing, _)) if *existing != arguments => {
                        return Err(format!("`{sub_path}` is selected twice with different arguments").into());
                    }
                    Some(_) => {}
                    None if arguments.is_empty() => {}
                    None => {
                        let get = |name: &str| arguments.iter().find(|(n, _)| n == name).map(|(_, v)| v.clone());
                        let parsed = crate::args::Arguments::parse(target, get)?.nested();
                        nested.push((sub_path.clone(), arguments, parsed));
                    }
                }
                selection.nested(view_field.name, self::selection(target, &sub, &sub_path, nested)?)
            }
            FieldKind::ToOne { shape: target, .. } => {
                selection.nested(view_field.name, self::selection(target(), &sub, &sub_path, nested)?)
            }
        };
    }
    Ok(selection)
}

/// `true` if every variant of an embedded enum is a unit variant, so it is a GraphQL enum.
pub(crate) fn unit_enum(kind: &EmbeddedKind) -> bool {
    match kind {
        EmbeddedKind::Sum(sum) => sum.variants.iter().all(|v| matches!(v.data, mabat::shape::VariantData::Unit)),
        EmbeddedKind::Product { .. } => false,
    }
}
