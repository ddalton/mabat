---
title: JSON and selections
description: Load views as JSON, whole or only the fields a client selects.
---

Any view also loads as `serde_json::Value` objects, whole or a **selection** of its fields. A selection is written
like a GraphQL selection set; only its columns are selected and only its child queries run
([MPA-JSON-3](../../spec/mpa/#mpa-json-3)).

```rust
use mabat::Selection;

let selection = Selection::parse("name assignee { name } children { name }")?;
let tasks = mabat::load::<TaskView>().select(selection).json(&mut conn).await?;
// [{ "name": "Release", "assignee": { "name": "Ada" }, "children": [{ "name": "Write docs" }] }]
```

## The JSON

- Columns use their Rust type's `Serialize`; a type without it fails only when loaded as JSON
  ([MPA-JSON-1](../../spec/mpa/#mpa-json-1), [MPA-JSON-2](../../spec/mpa/#mpa-json-2)).
- `#[view(json)]` columns are written as the JSON they hold.
- Collections are arrays, maps are objects, references are objects or `null`.
- Enums are objects whose `__typename` names the variant; tuple fields are `_0`, `_1`, …

## Selections

- A collection or reference selected by name alone loads its view's columns and embedded values, not its own
  collections or references, so every selection has a finite depth ([MPA-JSON-4](../../spec/mpa/#mpa-json-4)).
- Recursive and graph views load as trees as deep as the selection asks ([MPA-JSON-5](../../spec/mpa/#mpa-json-5)).
- Overrides apply to selections; typed terminals refuse them ([MPA-JSON-6](../../spec/mpa/#mpa-json-6)).
- Selections combine with filters, paging and [nested arguments](../loading/#nested-collections).
