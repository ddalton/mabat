//! Planning the queries that fill a view.
//!
//! A view is filled by a tree of queries. The root query selects the view's table,
//! including embedded structs, enums stored in the row, and the foreign keys of to-one
//! references. Each to-many collection and each to-one reference is loaded by a child query
//! keyed by the keys collected from its parent query, which avoids both N+1 queries and the
//! cartesian product of joining collections. So is each variant of an enum stored in a table
//! per variant.
//!
//! Every selected column is aliased with its path relative to the query's view, e.g.
//! `name`, `address.city` or `status.Blocked.reason`, plus the system aliases [`KEY_ALIAS`],
//! [`PARENT_ALIAS`], [`REF_ALIAS_PREFIX`] and [`TAG_ALIAS`]. Results are decoded by alias,
//! never by position.

use std::collections::HashSet;
use std::fmt::Write;

use crate::shape::{
    EmbeddedKind, EmbeddedShape, Field, FieldKind, OrderBy, SumShape, SumStrategy, VariantData, ViewShape,
};
use crate::{KEY_ALIAS, PARENT_ALIAS, REF_ALIAS_PREFIX, ROOT_QUERY, TAG_ALIAS};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    #[error("{view} at `{path}` refers back to {view}; recursive views are not supported yet")]
    Recursive { view: &'static str, path: String },
    #[error("{view}: field `{field}` is an embedded value that contains a {kind}, which is not supported")]
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
    /// The enums stored in the rows of this query, for strict decoding.
    pub sums: Vec<SumPlan>,
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
    /// A variant of an enum stored in a table per variant: rows whose key is the key of a
    /// parent row whose `tag_alias` column is `tag_value`.
    Variant { tag_alias: String, tag_value: &'static str },
}

#[derive(Debug)]
pub struct ChildPlan {
    /// Index of the field in the parent's [`ViewShape::fields`].
    pub field_index: usize,
    /// The variant, for a query of a variant table.
    pub variant: Option<&'static str>,
    pub plan: QueryPlan,
}

/// A selected column and its alias.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectColumn {
    pub column: String,
    pub alias: String,
    /// Select the column as `text`, for tag columns of any type, e.g. a PostgreSQL enum.
    pub as_text: bool,
}

/// An enum stored in the rows of a query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SumPlan {
    /// Prefix of the aliases of the enum, e.g. `status.`.
    pub alias_prefix: String,
    pub variants: Vec<VariantPlan>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariantPlan {
    pub name: &'static str,
    /// Aliases that must be NULL in a row of this variant: the columns of the other
    /// variants that this variant does not use. Empty for a lenient enum.
    pub exclusive: Vec<String>,
}

impl SelectColumn {
    fn new(column: impl Into<String>, alias: impl Into<String>) -> SelectColumn {
        SelectColumn { column: column.into(), alias: alias.into(), as_text: false }
    }
}

/// A query of a variant table found while adding the columns of a view.
struct VariantQuery {
    field_index: usize,
    path: String,
    tag_alias: String,
    variant: &'static str,
    tag_value: &'static str,
    shape: &'static ViewShape,
}

/// Collects the columns of the row of a query.
struct Row<'a> {
    view: &'static ViewShape,
    columns: &'a mut Vec<SelectColumn>,
    sums: &'a mut Vec<SumPlan>,
    variants: &'a mut Vec<VariantQuery>,
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

        let mut columns = vec![SelectColumn::new(shape.key_column, KEY_ALIAS)];
        if let Link::Child { fk } = &link {
            columns.push(SelectColumn::new(*fk, PARENT_ALIAS));
        }

        let mut sums = Vec::new();
        let mut variants = Vec::new();
        let mut children = Vec::new();
        for (field_index, field) in shape.fields.iter().enumerate() {
            let field_path = join_path(&path, field.name);
            match &field.kind {
                FieldKind::Column { column } => columns.push(SelectColumn::new(*column, field.name)),
                FieldKind::Embedded { column_prefix, shape: embedded } => {
                    let mut row = Row { view: shape, columns: &mut columns, sums: &mut sums, variants: &mut variants };
                    let top = Some((field_index, field_path.as_str()));
                    row.add_embedded(embedded(), column_prefix, &format!("{}.", field.name), top)?;
                }
                FieldKind::Child { fk, order_by, shape: child } => {
                    let plan = Self::build_inner(child(), field_path, Link::Child { fk }, order_by.to_vec(), stack)?;
                    children.push(ChildPlan { field_index, variant: None, plan });
                }
                FieldKind::ToOne { fk, shape: target, .. } => {
                    let ref_alias = format!("{REF_ALIAS_PREFIX}{}", field.name);
                    columns.push(SelectColumn::new(*fk, ref_alias.clone()));
                    let plan = Self::build_inner(target(), field_path, Link::ToOne { ref_alias }, Vec::new(), stack)?;
                    children.push(ChildPlan { field_index, variant: None, plan });
                }
            }
        }

        for query in variants {
            let link = Link::Variant { tag_alias: query.tag_alias, tag_value: query.tag_value };
            let plan = Self::build_inner(query.shape, query.path, link, Vec::new(), stack)?;
            children.push(ChildPlan { field_index: query.field_index, variant: Some(query.variant), plan });
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
        Ok(QueryPlan { shape, path, link, key_alias, columns, order_by, sums, children })
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
            Link::Variant { tag_alias, tag_value } => format!(" (variant where {tag_alias} = '{tag_value}')"),
        };
        let _ = writeln!(out, "{indent}{name}: {}{link}", self.shape.name);
        let _ = writeln!(out, "{indent}  {}", crate::sql::select(self, &crate::sql::RootOptions::default()));
        for child in &self.children {
            child.plan.explain_inner(out, depth + 1);
        }
    }
}

impl Row<'_> {
    /// Add the columns of an embedded struct or enum. `top` is the field index and path of
    /// the view's field when the value is not nested in another embedded value.
    fn add_embedded(
        &mut self,
        shape: &'static EmbeddedShape,
        column_prefix: &str,
        alias_prefix: &str,
        top: Option<(usize, &str)>,
    ) -> Result<(), PlanError> {
        match &shape.kind {
            EmbeddedKind::Product { fields } => {
                for field in *fields {
                    self.add_field(field, column_prefix, alias_prefix)?;
                }
                Ok(())
            }
            EmbeddedKind::Sum(sum) => self.add_sum(sum, column_prefix, alias_prefix, top),
        }
    }

    fn add_sum(
        &mut self,
        sum: &'static SumShape,
        column_prefix: &str,
        alias_prefix: &str,
        top: Option<(usize, &str)>,
    ) -> Result<(), PlanError> {
        let tag_alias = format!("{alias_prefix}{TAG_ALIAS}");
        self.columns.push(SelectColumn {
            column: format!("{column_prefix}{}", sum.tag_column),
            alias: tag_alias.clone(),
            as_text: true,
        });

        if sum.strategy == SumStrategy::TablePerVariant {
            let Some((field_index, path)) = top else {
                return Err(PlanError::UnsupportedEmbedded {
                    view: self.view.name,
                    field: alias_prefix.trim_end_matches('.').to_string(),
                    kind: "enum stored in a table per variant",
                });
            };
            for variant in sum.variants {
                if let VariantData::Table { shape } = &variant.data {
                    self.variants.push(VariantQuery {
                        field_index,
                        path: format!("{path}.{}", variant.name),
                        tag_alias: tag_alias.clone(),
                        variant: variant.name,
                        tag_value: variant.tag_value,
                        shape: shape(),
                    });
                }
            }
            return Ok(());
        }

        // The columns of each variant, to find the columns that must be NULL for the others
        let mut ranges = Vec::new();
        for variant in sum.variants {
            let start = self.columns.len();
            match &variant.data {
                VariantData::Unit => {}
                VariantData::Columns { fields } => {
                    let prefix = format!("{alias_prefix}{}.", variant.name);
                    for field in *fields {
                        self.add_field(field, column_prefix, &prefix)?;
                    }
                }
                VariantData::Table { .. } => {
                    return Err(PlanError::UnsupportedEmbedded {
                        view: self.view.name,
                        field: format!("{alias_prefix}{}", variant.name),
                        kind: "variant table in an enum stored in columns",
                    });
                }
            }
            ranges.push(start..self.columns.len());
        }

        let variants = sum
            .variants
            .iter()
            .zip(&ranges)
            .map(|(variant, own)| {
                let mut exclusive = Vec::new();
                if !sum.lenient {
                    let used: HashSet<&str> = self.columns[own.clone()].iter().map(|c| c.column.as_str()).collect();
                    for other in ranges.iter().filter(|r| *r != own) {
                        for column in &self.columns[other.clone()] {
                            if !used.contains(column.column.as_str()) && !exclusive.contains(&column.alias) {
                                exclusive.push(column.alias.clone());
                            }
                        }
                    }
                }
                VariantPlan { name: variant.name, exclusive }
            })
            .collect();
        self.sums.push(SumPlan { alias_prefix: alias_prefix.to_string(), variants });
        Ok(())
    }

    fn add_field(&mut self, field: &Field, column_prefix: &str, alias_prefix: &str) -> Result<(), PlanError> {
        match &field.kind {
            FieldKind::Column { column } => {
                self.columns.push(SelectColumn::new(
                    format!("{column_prefix}{column}"),
                    format!("{alias_prefix}{}", field.name),
                ));
                Ok(())
            }
            FieldKind::Embedded { column_prefix: inner, shape } => self.add_embedded(
                shape(),
                &format!("{column_prefix}{inner}"),
                &format!("{alias_prefix}{}.", field.name),
                None,
            ),
            FieldKind::Child { .. } => Err(PlanError::UnsupportedEmbedded {
                view: self.view.name,
                field: format!("{alias_prefix}{}", field.name),
                kind: "child collection",
            }),
            FieldKind::ToOne { .. } => Err(PlanError::UnsupportedEmbedded {
                view: self.view.name,
                field: format!("{alias_prefix}{}", field.name),
                kind: "to-one reference",
            }),
        }
    }
}

fn join_path(parent: &str, field: &str) -> String {
    if parent.is_empty() { field.to_string() } else { format!("{parent}.{field}") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shape::{SumShape, Variant};

    static PERSON_FIELDS: [Field; 1] = [Field { name: "name", kind: FieldKind::Column { column: "full_name" } }];
    static PERSON: ViewShape = ViewShape { name: "Person", table: "person", key_column: "id", fields: &PERSON_FIELDS };

    static ADDRESS_FIELDS: [Field; 2] = [
        Field { name: "street", kind: FieldKind::Column { column: "street" } },
        Field { name: "city", kind: FieldKind::Column { column: "city" } },
    ];
    static ADDRESS: EmbeddedShape =
        EmbeddedShape { name: "Address", kind: EmbeddedKind::Product { fields: &ADDRESS_FIELDS } };

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
    // enum Status { Open, Assigned { assignee: String }, Reassigned { assignee: String, by: String },
    //               Blocked { reason: Reason } } with a nested enum Reason { Waiting { on: String }, Other }
    static ASSIGNED_FIELDS: [Field; 1] = [Field { name: "assignee", kind: FieldKind::Column { column: "assignee" } }];
    static REASSIGNED_FIELDS: [Field; 2] = [
        Field { name: "assignee", kind: FieldKind::Column { column: "assignee" } },
        Field { name: "by", kind: FieldKind::Column { column: "reassigned_by" } },
    ];
    static WAITING_FIELDS: [Field; 1] = [Field { name: "on", kind: FieldKind::Column { column: "waiting_on" } }];
    static REASON_VARIANTS: [Variant; 2] = [
        Variant { name: "Waiting", tag_value: "waiting", data: VariantData::Columns { fields: &WAITING_FIELDS } },
        Variant { name: "Other", tag_value: "other", data: VariantData::Unit },
    ];
    static REASON: EmbeddedShape = EmbeddedShape {
        name: "Reason",
        kind: EmbeddedKind::Sum(SumShape {
            tag_column: "reason",
            strategy: SumStrategy::Tag,
            lenient: false,
            variants: &REASON_VARIANTS,
        }),
    };
    static BLOCKED_FIELDS: [Field; 1] =
        [Field { name: "reason", kind: FieldKind::Embedded { column_prefix: "", shape: || &REASON } }];
    static STATUS_VARIANTS: [Variant; 4] = [
        Variant { name: "Open", tag_value: "open", data: VariantData::Unit },
        Variant { name: "Assigned", tag_value: "assigned", data: VariantData::Columns { fields: &ASSIGNED_FIELDS } },
        Variant {
            name: "Reassigned",
            tag_value: "reassigned",
            data: VariantData::Columns { fields: &REASSIGNED_FIELDS },
        },
        Variant { name: "Blocked", tag_value: "blocked", data: VariantData::Columns { fields: &BLOCKED_FIELDS } },
    ];
    static STATUS: EmbeddedShape = EmbeddedShape {
        name: "Status",
        kind: EmbeddedKind::Sum(SumShape {
            tag_column: "kind",
            strategy: SumStrategy::Tag,
            lenient: false,
            variants: &STATUS_VARIANTS,
        }),
    };
    static ISSUE_FIELDS: [Field; 1] =
        [Field { name: "status", kind: FieldKind::Embedded { column_prefix: "status_", shape: || &STATUS } }];
    static ISSUE: ViewShape = ViewShape { name: "Issue", table: "issue", key_column: "id", fields: &ISSUE_FIELDS };

    #[test]
    fn enums_in_the_row() {
        let plan = QueryPlan::build(&ISSUE).unwrap();
        assert_eq!(
            aliases(&plan),
            [
                "$key",
                "status.$tag",
                "status.Assigned.assignee",
                "status.Reassigned.assignee",
                "status.Reassigned.by",
                "status.Blocked.reason.$tag",
                "status.Blocked.reason.Waiting.on",
            ]
        );
        let tag = &plan.columns[1];
        assert_eq!((tag.column.as_str(), tag.as_text), ("status_kind", true));
        assert_eq!(plan.columns[6].column, "status_waiting_on");
        assert_eq!(plan.query_count(), 1);

        let sql = crate::sql::select(&plan, &crate::sql::RootOptions::default());
        assert!(sql.contains("t0.\"status_kind\"::text AS \"status.$tag\""), "{sql}");
    }

    #[test]
    fn columns_of_other_variants_must_be_null() {
        let plan = QueryPlan::build(&ISSUE).unwrap();
        // Nested enums are planned before the enum that contains them
        let [reason, status] = plan.sums.as_slice() else { panic!("{:?}", plan.sums) };
        assert_eq!(reason.alias_prefix, "status.Blocked.reason.");
        assert_eq!(reason.variants[1].exclusive, ["status.Blocked.reason.Waiting.on"]);

        assert_eq!(status.alias_prefix, "status.");
        let exclusive = |name: &str| &status.variants.iter().find(|v| v.name == name).unwrap().exclusive;
        assert_eq!(
            exclusive("Open"),
            &[
                "status.Assigned.assignee",
                "status.Reassigned.assignee",
                "status.Reassigned.by",
                "status.Blocked.reason.$tag",
                "status.Blocked.reason.Waiting.on",
            ]
        );
        // The assignee column is shared by Assigned and Reassigned
        assert_eq!(
            exclusive("Assigned"),
            &["status.Reassigned.by", "status.Blocked.reason.$tag", "status.Blocked.reason.Waiting.on"]
        );
        assert_eq!(
            exclusive("Blocked"),
            &["status.Assigned.assignee", "status.Reassigned.assignee", "status.Reassigned.by"]
        );
    }

    // enum Payment { Card { last4 } in card_payment, Cash } stored in a table per variant
    static CARD_FIELDS: [Field; 1] = [Field { name: "last4", kind: FieldKind::Column { column: "last4" } }];
    static CARD: ViewShape =
        ViewShape { name: "Payment::Card", table: "card_payment", key_column: "payment_id", fields: &CARD_FIELDS };
    static PAYMENT_VARIANTS: [Variant; 2] = [
        Variant { name: "Card", tag_value: "card", data: VariantData::Table { shape: || &CARD } },
        Variant { name: "Cash", tag_value: "cash", data: VariantData::Unit },
    ];
    static PAYMENT: EmbeddedShape = EmbeddedShape {
        name: "Payment",
        kind: EmbeddedKind::Sum(SumShape {
            tag_column: "kind",
            strategy: SumStrategy::TablePerVariant,
            lenient: false,
            variants: &PAYMENT_VARIANTS,
        }),
    };
    static ORDER_FIELDS: [Field; 1] =
        [Field { name: "payment", kind: FieldKind::Embedded { column_prefix: "payment_", shape: || &PAYMENT } }];
    static ORDER_VIEW: ViewShape =
        ViewShape { name: "Order", table: "orders", key_column: "id", fields: &ORDER_FIELDS };

    #[test]
    fn variant_tables_are_child_queries() {
        let plan = QueryPlan::build(&ORDER_VIEW).unwrap();
        assert_eq!(aliases(&plan), ["$key", "payment.$tag"]);
        assert!(plan.sums.is_empty());
        let [card] = plan.children.as_slice() else { panic!() };
        assert_eq!((card.field_index, card.variant), (0, Some("Card")));
        assert_eq!(card.plan.query_name(), "payment.Card");
        assert_eq!(card.plan.link, Link::Variant { tag_alias: "payment.$tag".into(), tag_value: "card" });
        assert_eq!(aliases(&card.plan), ["$key", "last4"]);
        assert_eq!(
            crate::sql::select(&card.plan, &crate::sql::RootOptions::default()),
            "SELECT t0.\"payment_id\" AS \"$key\", t0.\"last4\" AS \"last4\" FROM \"card_payment\" AS t0 \
             WHERE t0.\"payment_id\" = ANY($1) ORDER BY t0.\"payment_id\""
        );
        assert!(plan.explain().contains("payment.Card: Payment::Card (variant where payment.$tag = 'card')"));
    }

    static WRAPPER_FIELDS: [Field; 1] =
        [Field { name: "payment", kind: FieldKind::Embedded { column_prefix: "", shape: || &PAYMENT } }];
    static WRAPPER: EmbeddedShape =
        EmbeddedShape { name: "Wrapper", kind: EmbeddedKind::Product { fields: &WRAPPER_FIELDS } };
    static WRAPPED_FIELDS: [Field; 1] =
        [Field { name: "wrapper", kind: FieldKind::Embedded { column_prefix: "", shape: || &WRAPPER } }];
    static WRAPPED: ViewShape =
        ViewShape { name: "Wrapped", table: "orders", key_column: "id", fields: &WRAPPED_FIELDS };

    #[test]
    fn variant_tables_need_to_be_fields_of_a_view() {
        let err = QueryPlan::build(&WRAPPED).unwrap_err();
        assert_eq!(
            err,
            PlanError::UnsupportedEmbedded {
                view: "Wrapped",
                field: "wrapper.payment".into(),
                kind: "enum stored in a table per variant"
            }
        );
    }
}
