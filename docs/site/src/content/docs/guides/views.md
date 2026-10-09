---
title: Declaring views
description: Every attribute of #[derive(View)] — tables, keys, columns, embedded values, references and collections.
---

A view is a Rust struct deriving `mabat::View`. It names its table, and each field is a column unless an attribute
makes it an embedded value, a reference or a collection ([MPA-CORE-1](../../spec/mpa/#mpa-core-1)).

```rust
use mabat::View;
use uuid::Uuid;

#[derive(View)]
#[view(table = "task")]                       // key = "id" by default
struct TaskView {
    id: Uuid,
    name: String,
    description: Option<String>,              // nullable: NULL is None
    #[view(column = "created")]
    created_at: chrono::DateTime<chrono::Utc>,
    #[view(json)]
    metadata: Option<Metadata>,               // any serde type, from a JSON or JSONB column
    #[view(embed(prefix = "addr_"))]
    address: Address,                         // columns addr_street, addr_city
    #[view(to_one(fk = "assignee_id"))]
    assignee: Option<PersonView>,             // a reference to another view
    #[view(child(fk = "parent_id", order_by = "position, name desc"))]
    children: Vec<SubtaskView>,               // a collection of another view
}

#[derive(View)]
#[view(embedded)]
struct Address {
    street: String,
    city: String,
}
```

## The view

| Attribute | Meaning | Rule |
| --- | --- | --- |
| `table = "t"` | The table the view is loaded from | [MPA-VIEW-1](../../spec/mpa/#mpa-view-1) |
| `key = "c"` | The key column, `id` by default | [MPA-VIEW-2](../../spec/mpa/#mpa-view-2) |
| `embedded` | A struct stored in the columns of the view that contains it | [MPA-VIEW-3](../../spec/mpa/#mpa-view-3) |
| `databases = "postgres, mysql"` | Limit the databases the view is generated for | [MPA-DB-2](../../spec/mpa/#mpa-db-2) |

The same table can have as many views as the use cases that read it: a list view with three columns and a detail
view with every relationship.

## Fields

- **Columns** are named like the field, unless `column` names them. `Option<T>` is nullable; a NULL in a
  non-`Option` field is a decode error that names the path ([MPA-VIEW-5](../../spec/mpa/#mpa-view-5)).
- **`json`** decodes a JSON column with `serde` ([MPA-VIEW-6](../../spec/mpa/#mpa-view-6)).
- **`embed`** holds an embedded struct or an [enum](../enums/), whose columns carry an optional prefix; embedded
  values nest and prefixes concatenate ([MPA-VIEW-7](../../spec/mpa/#mpa-view-7)).
- **`to_one(fk = "c")`** references another view through a foreign key of this table, held as `T`, `Box<T>`,
  `Arc<T>` or `Ref<T>`. `Option<T>` makes it optional; a required reference to a missing row is an error
  ([MPA-VIEW-8](../../spec/mpa/#mpa-view-8)). A reference to its own view, such as a parent, is
  [recursive](../collections/#chains-of-parents).
- **`child(fk = "c")`** is a to-many collection, described in [Collections and recursion](../collections/).
- **`version`** marks an integer column for [optimistic locking](../writing/#optimistic-locking).

Invalid combinations are compile errors that name the attribute: a `child` without `fk`, `index` on a map,
`version` on a collection ([MPA-VIEW-11](../../spec/mpa/#mpa-view-11)). The complete list is on the
[attributes reference](../../reference/attributes/).

## How fields are read

Every query aliases its columns with the field's path — `name`, `address.city`, `children.notes` — and rows are
decoded by alias, never by position ([MPA-PLAN-2](../../spec/mpa/#mpa-plan-2),
[MPA-PLAN-3](../../spec/mpa/#mpa-plan-3)). That is what lets an [override](../overrides/) select columns in any
order, from any table, as long as it keeps the aliases.
