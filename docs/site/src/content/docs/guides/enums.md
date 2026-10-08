---
title: Enums with data
description: Rust enums with data, stored in columns of the row or in a table per variant, decoded strictly.
---

Rust enums with data are first-class: a tag column names the variant, and the variant's data is stored either in
columns of the same row or in a table of its own ([MPA-SUM-1](../../spec/mpa/#mpa-sum-1)).

## In columns of the row

```rust
#[derive(View)]
#[view(tag = "state")]                        // any column type, such as a PostgreSQL or MySQL enum
enum State {
    #[view(tag_value = "open")]
    Open,
    #[view(tag_value = "assigned")]
    Assigned { assignee: String },
    #[view(tag_value = "blocked")]
    Blocked {
        #[view(embed(prefix = "blocked_reason_"))]
        reason: Reason,                       // enums nest
        #[view(column = "blocked_since")]
        since: Option<DateTime<Utc>>,
    },
    #[view(tag_value = "closed")]
    Closed(#[view(column = "closed_resolution")] String),
}

#[derive(View)]
#[view(table = "issue")]
struct IssueView {
    id: i64,
    #[view(embed)]
    state: State,
}
```

Tuple fields name their column. Variant fields have paths that name the variant, such as
`state.Blocked.reason.$tag` or `state.Closed.0`, which overrides use like any other path
([MPA-SUM-2](../../spec/mpa/#mpa-sum-2)).

## In a table per variant

```rust
#[derive(View)]
#[view(tag = "kind", strategy = "table_per_variant")]
enum Payment {
    #[view(tag_value = "none")]
    Unpaid,
    #[view(tag_value = "card", table = "card_payment", key = "issue_id")]
    Card { last4: String, #[view(to_one(fk = "holder_id"))] holder: Option<PersonView> },
    #[view(tag_value = "bank", table = "bank_payment", key = "issue_id")]
    Bank { iban: String, #[view(child(fk = "bank_payment_id"))] notes: Vec<PaymentNote> },
}
```

Each variant table is keyed by the containing view's key and loaded by one batched query with only the keys whose
tag names that variant. Variant tables can have references and collections of their own
([MPA-SUM-3](../../spec/mpa/#mpa-sum-3)).

## Strict decoding

A NULL tag, an unknown tag and a missing variant row are errors, and so is a non-NULL column of another variant —
unless the enum is marked `lenient` ([MPA-SUM-4](../../spec/mpa/#mpa-sum-4)). Data that does not fit the enum
never decodes silently into the wrong variant.

## Writing

[Saving](../writing/) writes the tag and the variant's columns and NULLs the other variants' columns, or saves the
variant's table row and deletes the other variants' rows ([MPA-WRITE-6](../../spec/mpa/#mpa-write-6)).
