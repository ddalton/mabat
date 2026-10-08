---
title: Collections and recursion
description: Ordered lists, maps, many-to-many collections and recursive trees.
---

A `child` field is a collection of another view, loaded by one query for all parents
([MPA-VIEW-9](../../spec/mpa/#mpa-view-9)).

```rust
#[derive(View)]
#[view(table = "playlist")]
struct PlaylistView {
    name: String,
    // ordered by columns; the key always breaks ties
    #[view(child(fk = "playlist_id", order_by = "added_at desc"))]
    comments: Vec<CommentView>,
    // many-to-many through a link table, placed by its index column whatever the row order
    #[view(child(through = "playlist_song", fk = "playlist_id", target = "song_id", index = "seq"))]
    songs: Vec<SongView>,
    // a map keyed by a column (BTreeMap or HashMap)
    #[view(child(fk = "playlist_id", key = "name"))]
    settings: BTreeMap<String, SettingView>,
}
```

| Argument | Meaning |
| --- | --- |
| `fk = "c"` | The column of the child table (or of the link table) referencing this view's key |
| `order_by = "a, b desc"` | The order of the elements |
| `through = "link"`, `target = "c"` | A many-to-many collection through a link table |
| `index = "c"` | Place the elements of a `Vec` by an integer column: non-NULL and distinct, gaps allowed |
| `key = "c"` | Key the elements of a map by a column; two elements with one key are an error |
| `depth = n` | A recursive collection, at most `n` levels |
| `recursive = "cte"` | A recursive collection loaded with one `WITH RECURSIVE` query |

A collection is a `Vec<T>`, `Vec<Arc<T>>`, `Vec<Ref<T>>`, or a `BTreeMap`/`HashMap` of owned views
([MPA-VIEW-10](../../spec/mpa/#mpa-view-10)).

## Recursive trees

```rust
#[derive(View)]
#[view(table = "category")]
struct CategoryTree {
    name: String,
    // one batched query per level, at most 5 levels
    #[view(child(fk = "parent_id", index = "position", depth = 5))]
    children: Vec<CategoryTree>,
}

#[derive(View)]
#[view(table = "category")]
struct CategoryCte {
    name: String,
    // every level in one WITH RECURSIVE query
    #[view(child(fk = "parent_id", order_by = "name", recursive = "cte"))]
    children: Vec<CategoryCte>,
}
```

Recursive views are owned trees: no `Rc`, no `RefCell` ([MPA-PLAN-4](../../spec/mpa/#mpa-plan-4)).

- A recursive collection has one query name, such as `children`, so one override tunes every level.
- With `recursive = "cte"`, the collections under the recursive view load with one query for all levels.
- Rows whose parents form a cycle cannot be a tree: loading them is an error, not an endless loop
  ([MPA-LOAD-12](../../spec/mpa/#mpa-load-12)).

## Filtering and paging a collection

`Load::nested` filters, orders and pages the elements of each parent, still in the collection's one query — see
[Loading](../loading/#nested-collections).
