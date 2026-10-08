//! Planning the queries that fill a view.
//!
//! A view is filled by a tree of queries. The root query selects the view's table,
//! including embedded structs and the foreign keys of to-one references. Each to-many
//! collection and each to-one reference is loaded by a child query keyed by the keys
//! collected from its parent query, which avoids both N+1 queries and the cartesian
//! product of joining collections.
//!
//! Every selected column is aliased with its path relative to the query's view, e.g.
//! `name` or `address.city`, plus the system aliases [`KEY_ALIAS`], [`PARENT_ALIAS`] and
//! [`REF_ALIAS_PREFIX`]. Results are decoded by alias, never by position.

use std::fmt::Write;

use crate::shape::{EmbeddedShape, Field, FieldKind, OrderBy, ViewShape};
use crate::{KEY_ALIAS, PARENT_ALIAS, REF_ALIAS_PREFIX, ROOT_QUERY};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    #[error("{view} at `{path}` refers back to {view}; recursive views are not supported yet")]
    Recursive { view: &'static str, path: String },
    #[error("{view}: field `{field}` is an embedded struct that contains a {kind}, which is not supported")]
    UnsupportedEmbedded { view: &'static str, field: String, kind: &'static str },
}

/// A query in the plan of a view.
#[derive(Debug)]
pub struct QueryPlan {
    pub shape: &'static ViewShape,
    /// Path of the view within the root view, empty for the root.
    pub path: String,
    pub link: Link,
    /// Alias of the key column: the alias of the field that holds the key, or [`KEY_ALIAS`]
    /// if the view has no such field.
    pub key_alias: String,
    pub columns: Vec<SelectColumn>,
    /// Ordering of the rows of a child query.
    pub order_by: Vec<OrderBy>,
    pub children: Vec<ChildPlan>,
}

/// How a query is linked to its parent query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    /// The root query of the view.
    Root,
    /// A to-many collection: child rows whose `fk` column is one of the parent keys.
    Child { fk: &'static str },
    /// A to-one reference: rows whose key is one of the values of the parent's
    /// `ref_alias` column.
    ToOne { ref_alias: String },
}

#[derive(Debug)]
pub struct ChildPlan {
    /// Index of the field in the parent's [`ViewShape::fields`].
    pub field_index: usize,
    pub plan: QueryPlan,
}

/// A selected column and its alias.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectColumn {
    pub column: String,
    pub alias: String,
}

impl QueryPlan {
    /// Plan the queries for a view.
    pub fn build(shape: &'static ViewShape) -> Result<QueryPlan, PlanError> {
        let mut stack = Vec::new();
        Self::build_inner(shape, String::new(), Link::Root, Vec::new(), &mut stack)
    }

    fn build_inner(
        shape: &'static ViewShape,
        path: String,
        link: Link,
        order_by: Vec<OrderBy>,
        stack: &mut Vec<&'static ViewShape>,
    ) -> Result<QueryPlan, PlanError> {
        if stack.iter().any(|s| std::ptr::eq(*s, shape)) {
            return Err(PlanError::Recursive { view: shape.name, path });
        }
        stack.push(shape);

        let mut columns = vec![SelectColumn { column: shape.key_column.to_string(), alias: KEY_ALIAS.to_string() }];
        if let Link::Child { fk } = &link {
            columns.push(SelectColumn { column: fk.to_string(), alias: PARENT_ALIAS.to_string() });
        }

        let mut children = Vec::new();
        for (field_index, field) in shape.fields.iter().enumerate() {
            let field_path = join_path(&path, field.name);
            match &field.kind {
                FieldKind::Column { column } => {
                    columns.push(SelectColumn { column: column.to_string(), alias: field.name.to_string() });
                }
                FieldKind::Embedded { column_prefix, shape: embedded } => {
                    add_embedded_columns(shape, &mut columns, embedded(), column_prefix, &format!("{}.", field.name))?;
                }
                FieldKind::Child { fk, order_by, shape: child } => {
                    let plan = Self::build_inner(child(), field_path, Link::Child { fk }, order_by.to_vec(), stack)?;
                    children.push(ChildPlan { field_index, plan });
                }
                FieldKind::ToOne { fk, shape: target, .. } => {
                    let ref_alias = format!("{REF_ALIAS_PREFIX}{}", field.name);
                    columns.push(SelectColumn { column: fk.to_string(), alias: ref_alias.clone() });
                    let plan = Self::build_inner(target(), field_path, Link::ToOne { ref_alias }, Vec::new(), stack)?;
                    children.push(ChildPlan { field_index, plan });
                }
            }
        }

        // Select the key column only once when a field holds it
        let mut key_alias = KEY_ALIAS.to_string();
        let key_field =
            shape.fields.iter().find(|f| matches!(f.kind, FieldKind::Column { column } if column == shape.key_column));
        if let Some(field) = key_field {
            columns.remove(0);
            key_alias = field.name.to_string();
        }

        stack.pop();
        Ok(QueryPlan { shape, path, link, key_alias, columns, order_by, children })
    }

    /// Name of the query in its plan: its path, or `$root` for the root query. Overrides
    /// address queries by this name.
    pub fn query_name(&self) -> &str {
        if self.path.is_empty() { ROOT_QUERY } else { &self.path }
    }

    /// Visit this query and all queries below it, parents before children.
    pub fn walk<'a>(&'a self, visit: &mut impl FnMut(&'a QueryPlan)) {
        visit(self);
        for child in &self.children {
            child.plan.walk(visit);
        }
    }

    /// Number of queries in the plan, including this one.
    pub fn query_count(&self) -> usize {
        1 + self.children.iter().map(|c| c.plan.query_count()).sum::<usize>()
    }

    /// A readable description of the plan, with the SQL of every query.
    pub fn explain(&self) -> String {
        let mut out = String::new();
        self.explain_inner(&mut out, 0);
        out
    }

    fn explain_inner(&self, out: &mut String, depth: usize) {
        let indent = "  ".repeat(depth);
        let name = self.query_name();
        let link = match &self.link {
            Link::Root => String::new(),
            Link::Child { fk } => format!(" (to-many by {fk})"),
            Link::ToOne { ref_alias } => format!(" (to-one by {ref_alias})"),
        };
        let _ = writeln!(out, "{indent}{name}: {}{link}", self.shape.name);
        let _ = writeln!(out, "{indent}  {}", crate::sql::select(self, &crate::sql::RootOptions::default()));
        for child in &self.children {
            child.plan.explain_inner(out, depth + 1);
        }
    }
}

fn add_embedded_columns(
    view: &'static ViewShape,
    columns: &mut Vec<SelectColumn>,
    shape: &'static EmbeddedShape,
    column_prefix: &str,
    alias_prefix: &str,
) -> Result<(), PlanError> {
    for field in shape.fields {
        add_embedded_field(view, columns, field, column_prefix, alias_prefix)?;
    }
    Ok(())
}

fn add_embedded_field(
    view: &'static ViewShape,
    columns: &mut Vec<SelectColumn>,
    field: &Field,
    column_prefix: &str,
    alias_prefix: &str,
) -> Result<(), PlanError> {
    match &field.kind {
        FieldKind::Column { column } => {
            columns.push(SelectColumn {
                column: format!("{column_prefix}{column}"),
                alias: format!("{alias_prefix}{}", field.name),
            });
            Ok(())
        }
        FieldKind::Embedded { column_prefix: inner, shape } => add_embedded_columns(
            view,
            columns,
            shape(),
            &format!("{column_prefix}{inner}"),
            &format!("{alias_prefix}{}.", field.name),
        ),
        FieldKind::Child { .. } => Err(PlanError::UnsupportedEmbedded {
            view: view.name,
            field: format!("{alias_prefix}{}", field.name),
            kind: "child collection",
        }),
        FieldKind::ToOne { .. } => Err(PlanError::UnsupportedEmbedded {
            view: view.name,
            field: format!("{alias_prefix}{}", field.name),
            kind: "to-one reference",
        }),
    }
}

fn join_path(parent: &str, field: &str) -> String {
    if parent.is_empty() { field.to_string() } else { format!("{parent}.{field}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    static PERSON_FIELDS: [Field; 1] = [Field { name: "name", kind: FieldKind::Column { column: "full_name" } }];
    static PERSON: ViewShape = ViewShape { name: "Person", table: "person", key_column: "id", fields: &PERSON_FIELDS };

    static ADDRESS_FIELDS: [Field; 2] = [
        Field { name: "street", kind: FieldKind::Column { column: "street" } },
        Field { name: "city", kind: FieldKind::Column { column: "city" } },
    ];
    static ADDRESS: EmbeddedShape = EmbeddedShape { name: "Address", fields: &ADDRESS_FIELDS };

    static CHILD_ORDER: [OrderBy; 1] = [OrderBy::desc("name")];
    static TASK_FIELDS: [Field; 4] = [
        Field { name: "name", kind: FieldKind::Column { column: "name" } },
        Field { name: "address", kind: FieldKind::Embedded { column_prefix: "addr_", shape: || &ADDRESS } },
        Field { name: "assignee", kind: FieldKind::ToOne { fk: "assignee_id", optional: true, shape: || &PERSON } },
        Field {
            name: "children",
            kind: FieldKind::Child { fk: "parent_id", order_by: &CHILD_ORDER, shape: || &SUBTASK },
        },
    ];
    static TASK: ViewShape = ViewShape { name: "Task", table: "task", key_column: "id", fields: &TASK_FIELDS };

    static SUBTASK_FIELDS: [Field; 1] = [Field { name: "name", kind: FieldKind::Column { column: "name" } }];
    static SUBTASK: ViewShape = ViewShape { name: "Subtask", table: "task", key_column: "id", fields: &SUBTASK_FIELDS };

    static LOOP_FIELDS: [Field; 1] =
        [Field { name: "children", kind: FieldKind::Child { fk: "parent_id", order_by: &[], shape: || &LOOP } }];
    static LOOP: ViewShape = ViewShape { name: "Loop", table: "task", key_column: "id", fields: &LOOP_FIELDS };

    fn aliases(plan: &QueryPlan) -> Vec<&str> {
        plan.columns.iter().map(|c| c.alias.as_str()).collect()
    }

    #[test]
    fn root_columns() {
        let plan = QueryPlan::build(&TASK).unwrap();
        assert_eq!(plan.link, Link::Root);
        assert_eq!(aliases(&plan), ["$key", "name", "address.street", "address.city", "$ref.assignee"]);
        assert_eq!(plan.columns[2].column, "addr_street");
        assert_eq!(plan.query_count(), 3);
    }

    #[test]
    fn child_queries() {
        let plan = QueryPlan::build(&TASK).unwrap();
        let assignee = &plan.children[0];
        assert_eq!(assignee.field_index, 2);
        assert_eq!(assignee.plan.path, "assignee");
        assert_eq!(assignee.plan.link, Link::ToOne { ref_alias: "$ref.assignee".into() });
        assert_eq!(aliases(&assignee.plan), ["$key", "name"]);

        let children = &plan.children[1];
        assert_eq!(children.field_index, 3);
        assert_eq!(children.plan.link, Link::Child { fk: "parent_id" });
        assert_eq!(aliases(&children.plan), ["$key", "$parent", "name"]);
        assert_eq!(children.plan.order_by, [OrderBy::desc("name")]);
    }

    static KEYED_FIELDS: [Field; 2] = [
        Field { name: "code", kind: FieldKind::Column { column: "code" } },
        Field { name: "label", kind: FieldKind::Column { column: "label" } },
    ];
    static KEYED: ViewShape = ViewShape { name: "Keyed", table: "tag", key_column: "code", fields: &KEYED_FIELDS };

    #[test]
    fn key_field_is_selected_once() {
        let plan = QueryPlan::build(&KEYED).unwrap();
        assert_eq!(plan.key_alias, "code");
        assert_eq!(aliases(&plan), ["code", "label"]);

        let plan = QueryPlan::build(&TASK).unwrap();
        assert_eq!(plan.key_alias, "$key");
    }

    #[test]
    fn recursion_is_rejected() {
        let err = QueryPlan::build(&LOOP).unwrap_err();
        assert_eq!(err, PlanError::Recursive { view: "Loop", path: "children".into() });
    }

    #[test]
    fn explain_lists_every_query() {
        let explain = QueryPlan::build(&TASK).unwrap().explain();
        assert!(explain.contains("$root: Task"), "{explain}");
        assert!(explain.contains("assignee: Person (to-one by $ref.assignee)"), "{explain}");
        assert!(explain.contains("children: Subtask (to-many by parent_id)"), "{explain}");
    }

    #[test]
    fn query_names() {
        let plan = QueryPlan::build(&TASK).unwrap();
        let mut names = Vec::new();
        plan.walk(&mut |p| names.push(p.query_name().to_string()));
        assert_eq!(names, ["$root", "assignee", "children"]);
    }
}
