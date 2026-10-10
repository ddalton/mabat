//! Generic embedded structs: `#[view(embedded)] struct Range<T> { start: T, end: T }`.
//!
//! Rust has no generic statics, so the shape of each instantiation, `Range<NaiveDate>` or
//! `Range<i32>`, is built the first time it is asked for and kept for the life of the program.

use std::any::TypeId;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use mabat_core::{EmbeddedShape, Field};
use sqlx::{Decode, Encode, Type};

use crate::backend::Backend;

/// What a field typed by a type parameter of a generic embedded struct needs on database `B`:
/// to be decoded, encoded and described, written as JSON, and compared and cloned for
/// [`save_changes`](crate::save_changes) and [`save_all`](crate::save_all). Implemented for
/// every type that has all of these, such as `i32`, `String`, `Option<NaiveDate>` (with
/// chrono's `serde` feature) or `Decimal`.
pub trait GenericColumn<B: Backend>:
    for<'r> Decode<'r, B> + for<'t> Encode<'t, B> + Type<B> + serde::Serialize + Clone + PartialEq + Send + Sync + 'static
{
}

impl<T, B: Backend> GenericColumn<B> for T where
    T: for<'r> Decode<'r, B>
        + for<'t> Encode<'t, B>
        + Type<B>
        + serde::Serialize
        + Clone
        + PartialEq
        + Send
        + Sync
        + 'static
{
}

/// The shape of the instantiation `S` of a generic embedded struct, built by `build` the first
/// time.
pub fn generic_shape<S: 'static>(build: impl FnOnce() -> EmbeddedShape) -> &'static EmbeddedShape {
    static SHAPES: OnceLock<Mutex<HashMap<TypeId, &'static EmbeddedShape>>> = OnceLock::new();
    let shapes = SHAPES.get_or_init(Mutex::default);
    let id = TypeId::of::<S>();
    if let Some(shape) = shapes.lock().unwrap_or_else(|e| e.into_inner()).get(&id) {
        return shape;
    }
    // Built without the lock; if two threads build it, the first one kept is used
    let shape: &'static EmbeddedShape = Box::leak(Box::new(build()));
    let kept: &'static EmbeddedShape = shapes.lock().unwrap_or_else(|e| e.into_inner()).entry(id).or_insert(shape);
    kept
}

/// The name of the instantiation `S`, such as `Range<NaiveDate>`.
pub fn generic_name<S: ?Sized>() -> &'static str {
    Box::leak(crate::describe::short_type_name(std::any::type_name::<S>()).into_boxed_str())
}

/// The fields of a generic embedded struct's shape, kept for the life of the program.
pub fn leak_fields(fields: Vec<Field>) -> &'static [Field] {
    Box::leak(fields.into_boxed_slice())
}
