//! The GraphQL types of views, generated from their shapes.
//!
//! - A view or an embedded struct is an object type named after its Rust type, with a
//!   field per Rust field.
//! - An enum with data is a union of an object type per variant, named after the enum and
//!   the variant, with a `_variant` field and the fields of the variant. An enum without
//!   data is a GraphQL enum.
//! - A map collection is a list of `{ key, value }` objects.
//! - Columns are scalars: the built-in ones for booleans, 32-bit integers, floats and
//!   strings, and custom scalars for the others, see [`scalar_name`].

use std::collections::{BTreeSet, HashMap};

use async_graphql::dynamic::{Enum, Field, InputObject, InputValue, Object, Type, TypeRef, Union};
use mabat::shape::{
    EmbeddedKind, EmbeddedShape, Field as ViewField, FieldKind, Scalar, ValueType, VariantData, ViewShape,
};

use crate::resolve::Resolve;

/// The name of the enum of the sort directions.
pub(crate) const SORT_DIRECTION: &str = "SortDirection";

/// The GraphQL types of the views of a schema.
#[derive(Default)]
pub(crate) struct Types {
    types: Vec<Type>,
    /// The shape each type name was generated for, to find two views with the same name.
    names: HashMap<String, usize>,
    scalars: BTreeSet<String>,
    filters: BTreeSet<String>,
    pub(crate) errors: Vec<String>,
}

/// A GraphQL name for a Rust type name: without `::` and other characters GraphQL names do
/// not allow.
pub(crate) fn type_name(rust: &str) -> String {
    rust.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').collect()
}

/// The GraphQL name of a field: its name, or `_0`, `_1`, … for a tuple field.
pub(crate) fn field_name(field: &ViewField) -> String {
    if field.name.starts_with(|c: char| c.is_ascii_digit()) {
        format!("_{}", field.name)
    } else {
        field.name.to_string()
    }
}

/// The name of the GraphQL scalar of a column: `Boolean`, `Int`, `Float` and `String`, or
/// the custom scalars `BigInt` (64-bit integers), `UUID`, `Date`, `Time`, `DateTime` (with a
/// time zone), `NaiveDateTime`, `Decimal`, `JSON`, `Bytes`, and for other Rust types a
/// scalar named after the type.
pub(crate) fn scalar_name(scalar: Scalar) -> String {
    match scalar {
        Scalar::Boolean => "Boolean",
        Scalar::Int => "Int",
        Scalar::Float => "Float",
        Scalar::String => "String",
        Scalar::BigInt => "BigInt",
        Scalar::Uuid => "UUID",
        Scalar::Date => "Date",
        Scalar::Time => "Time",
        Scalar::DateTime => "DateTime",
        Scalar::NaiveDateTime => "NaiveDateTime",
        Scalar::Decimal => "Decimal",
        Scalar::Json => "JSON",
        Scalar::Bytes => "Bytes",
        Scalar::Other(name) => return type_name(name),
    }
    .to_string()
}

impl Types {
    /// Claim a type name for the shape at `address`: `false` if it is already generated.
    fn claim(&mut self, name: &str, address: usize, rust: &str) -> bool {
        match self.names.get(name) {
            Some(&existing) if existing == address => false,
            Some(_) => {
                self.errors
                    .push(format!("two views or embedded types are named {rust}; GraphQL type names must differ"));
                false
            }
            None => {
                self.names.insert(name.to_string(), address);
                true
            }
        }
    }

    /// The type of a column, registering its custom scalar.
    fn scalar(&mut self, ty: ValueType) -> TypeRef {
        let name = scalar_name(ty.scalar);
        if !matches!(name.as_str(), "Boolean" | "Int" | "Float" | "String") && self.scalars.insert(name.clone()) {
            self.types.push(async_graphql::dynamic::Scalar::new(name.clone()).into());
        }
        match (ty.list, ty.nullable) {
            (false, false) => TypeRef::named_nn(name),
            (false, true) => TypeRef::named(name),
            (true, false) => TypeRef::named_nn_list_nn(name),
            (true, true) => TypeRef::named_nn_list(name),
        }
    }

    /// The object type of a view, generated with the types it refers to.
    pub(crate) fn view(&mut self, shape: &'static ViewShape) -> String {
        let name = type_name(shape.name);
        if self.claim(&name, std::ptr::from_ref(shape) as usize, shape.name) {
            let object = self.fields(Object::new(&name), shape.fields);
            self.types.push(object.into());
        }
        name
    }

    /// The type of an embedded struct or enum, and how its fields resolve.
    fn embedded(&mut self, shape: &'static EmbeddedShape) -> (String, Resolve) {
        let name = type_name(shape.name);
        let resolve = match &shape.kind {
            EmbeddedKind::Product { .. } => Resolve::Object,
            kind if crate::resolve::unit_enum(kind) => Resolve::Enum,
            EmbeddedKind::Sum(_) => Resolve::Union { prefix: name.clone() },
        };
        if !self.claim(&name, std::ptr::from_ref(shape) as usize, shape.name) {
            return (name, resolve);
        }
        match &shape.kind {
            EmbeddedKind::Product { fields } => {
                let object = self.fields(Object::new(&name), fields);
                self.types.push(object.into());
            }
            EmbeddedKind::Sum(sum) if matches!(resolve, Resolve::Enum) => {
                self.types.push(Enum::new(&name).items(sum.variants.iter().map(|v| v.name)).into());
            }
            EmbeddedKind::Sum(sum) => {
                let mut union = Union::new(&name);
                for variant in sum.variants {
                    let object_name = format!("{name}{}", variant.name);
                    let variant_field = Field::new(
                        "_variant",
                        TypeRef::named_nn(TypeRef::STRING),
                        Resolve::Variant.resolver(String::new()),
                    );
                    let object = Object::new(&object_name).field(variant_field);
                    let object = match &variant.data {
                        VariantData::Unit => object,
                        VariantData::Columns { fields } => self.fields(object, fields),
                        VariantData::Table { shape } => self.fields(object, shape().fields),
                    };
                    self.types.push(object.into());
                    union = union.possible_type(object_name);
                }
                self.types.push(union.into());
            }
        }
        (name, resolve)
    }

    /// Add a field per field of a view, struct or variant to `object`.
    fn fields(&mut self, mut object: Object, fields: &'static [ViewField]) -> Object {
        for field in fields {
            let name = field_name(field);
            let (ty, resolve) = match &field.kind {
                FieldKind::Column { ty, .. } | FieldKind::Computed { ty } => (self.scalar(*ty), Resolve::Scalar),
                FieldKind::Embedded { shape, .. } => {
                    let (type_name, resolve) = self.embedded(shape());
                    (TypeRef::named_nn(type_name), resolve)
                }
                FieldKind::Child(child) => {
                    let target = self.view((child.shape)());
                    match child.map_key {
                        Some(_) => (TypeRef::named_nn_list_nn(self.entry(&target)), Resolve::Entries),
                        None => {
                            // A list takes the arguments of a root list, for the elements of each parent
                            let (where_name, order_name) =
                                (self.where_input((child.shape)()), self.order_input((child.shape)()));
                            let field = Field::new(
                                name.clone(),
                                TypeRef::named_nn_list_nn(target),
                                Resolve::List.resolver(name.clone()),
                            )
                            .argument(InputValue::new("where", TypeRef::named(where_name)))
                            .argument(InputValue::new("orderBy", TypeRef::named_nn_list(order_name)))
                            .argument(InputValue::new("limit", TypeRef::named(TypeRef::INT)))
                            .argument(InputValue::new("offset", TypeRef::named(TypeRef::INT)));
                            object = object.field(field);
                            continue;
                        }
                    }
                }
                FieldKind::ToOne { shape, optional, .. } => {
                    let target = self.view(shape());
                    let ty = if *optional { TypeRef::named(target) } else { TypeRef::named_nn(target) };
                    (ty, Resolve::Object)
                }
            };
            object = object.field(Field::new(name.clone(), ty, resolve.resolver(name)));
        }
        object
    }

    /// The type of the entries of a map of `target` values: `{ key: String!, value: target! }`.
    fn entry(&mut self, target: &str) -> String {
        let name = format!("{target}Entry");
        if self.names.insert(name.clone(), 0).is_none() {
            let object = Object::new(&name)
                .field(Field::new("key", TypeRef::named_nn(TypeRef::STRING), Resolve::Scalar.resolver("key".into())))
                .field(Field::new("value", TypeRef::named_nn(target), Resolve::Object.resolver("value".into())));
            self.types.push(object.into());
        }
        name
    }

    /// The input type of the `where` argument of a view: a filter per column of a scalar that
    /// can be compared, and `and`, `or` and `not`.
    pub(crate) fn where_input(&mut self, shape: &'static ViewShape) -> String {
        let name = format!("{}Where", type_name(shape.name));
        if self.names.insert(name.clone(), 0).is_some() {
            return name;
        }
        let mut input = InputObject::new(&name)
            .field(InputValue::new("and", TypeRef::named_nn_list(&name)))
            .field(InputValue::new("or", TypeRef::named_nn_list(&name)))
            .field(InputValue::new("not", TypeRef::named(&name)));
        for field in shape.fields {
            if let FieldKind::Column { ty, .. } = &field.kind
                && let Some(filter) = self.filter(*ty)
            {
                input = input.field(InputValue::new(field_name(field), TypeRef::named(filter)));
            }
        }
        self.types.push(input.into());
        name
    }

    /// The filter input of a column type, `None` if its values cannot be compared.
    fn filter(&mut self, ty: ValueType) -> Option<String> {
        if ty.list || !crate::args::comparable(ty.scalar) {
            return None;
        }
        let scalar = scalar_name(ty.scalar);
        let name = format!("{scalar}Filter");
        if self.filters.insert(name.clone()) {
            self.scalar(ValueType::new(ty.scalar));
            let mut input = InputObject::new(&name)
                .field(InputValue::new("eq", TypeRef::named(&scalar)))
                .field(InputValue::new("ne", TypeRef::named(&scalar)))
                .field(InputValue::new("isNull", TypeRef::named(TypeRef::BOOLEAN)));
            if ty.scalar != Scalar::Boolean {
                for op in ["lt", "le", "gt", "ge"] {
                    input = input.field(InputValue::new(op, TypeRef::named(&scalar)));
                }
                input = input
                    .field(InputValue::new("in", TypeRef::named_nn_list(&scalar)))
                    .field(InputValue::new("notIn", TypeRef::named_nn_list(&scalar)));
            }
            if ty.scalar == Scalar::String {
                input = input
                    .field(InputValue::new("like", TypeRef::named(TypeRef::STRING)))
                    .field(InputValue::new("ilike", TypeRef::named(TypeRef::STRING)));
            }
            self.types.push(input.into());
        }
        Some(name)
    }

    /// The input type of the `orderBy` argument of a view: a direction per sortable column.
    pub(crate) fn order_input(&mut self, shape: &'static ViewShape) -> String {
        let name = format!("{}OrderBy", type_name(shape.name));
        if self.names.insert(name.clone(), 0).is_some() {
            return name;
        }
        if self.names.insert(SORT_DIRECTION.to_string(), 0).is_none() {
            self.types.push(Enum::new(SORT_DIRECTION).items(["ASC", "DESC"]).into());
        }
        let mut input = InputObject::new(&name);
        for field in shape.fields {
            if let FieldKind::Column { ty, .. } = &field.kind
                && !ty.list
                && !matches!(ty.scalar, Scalar::Json | Scalar::Bytes)
            {
                input = input.field(InputValue::new(field_name(field), TypeRef::named(SORT_DIRECTION)));
            }
        }
        self.types.push(input.into());
        name
    }

    /// The type of the key of a view: its key field's, or `ID` when no field holds the key.
    pub(crate) fn key(&mut self, shape: &'static ViewShape) -> (TypeRef, Option<Scalar>) {
        let field = shape.fields.iter().find_map(|f| match f.kind {
            FieldKind::Column { column, ty } if column == shape.key_column && !ty.list => Some(ty),
            _ => None,
        });
        match field {
            Some(ty) => (self.scalar(ValueType::new(ty.scalar)), Some(ty.scalar)),
            None => (TypeRef::named_nn(TypeRef::ID), None),
        }
    }

    pub(crate) fn into_types(self) -> Vec<Type> {
        self.types
    }
}
