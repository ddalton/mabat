---
title: Shared values and graphs
description: Arc<T> values shared within a load, and Ref<T> graphs for cyclic data, with no Rc or RefCell.
---

## Shared values

A field of type `Arc<T>` — a reference or the elements of a collection — is decoded once per entity of a load and
shared: every task with the same assignee holds the same `Arc` ([MPA-LOAD-13](../../spec/mpa/#mpa-load-13)).

```rust
#[view(to_one(fk = "assignee_id"))]
assignee: Option<Arc<PersonView>>,
```

## Graphs

Cyclic data — a manager and their reports, a team and its members — uses `Ref<T>` fields. A `Ref<T>` is a typed,
`Copy` index into a `Graph`, and the derive generates a method per reference to follow it
([MPA-LOAD-14](../../spec/mpa/#mpa-load-14)).

```rust
#[derive(View)]
#[view(table = "employee")]
pub struct Employee {
    pub name: String,
    #[view(to_one(fk = "manager_id"))]
    pub manager: Option<Ref<Employee>>,
    #[view(child(fk = "manager_id", order_by = "name"))]
    pub reports: Vec<Ref<Employee>>,
    #[view(to_one(fk = "team_id"))]
    pub team: Ref<Team>,
}

let graph = mabat::load::<Employee>().by_key(id).graph(&mut conn).await?;
let me = graph.root().unwrap();
for colleague in me.manager(&graph).unwrap().reports(&graph) {
    println!("{} in {}", colleague.name, colleague.team(&graph).name);
}
```

- A graph load fetches every entity reachable through the `Ref` fields, each once, with batched queries. Cycles end
  by themselves; graphs need no `depth`.
- Navigation borrows the `Graph`: no runtime borrow checks. Changes go through `graph.get_mut(r)`.
- `Graph` is `Send + Sync`.
- A view with `Ref` fields is loaded with `graph`, or as [JSON with a selection](../json/), which unrolls it into a
  tree as deep as the selection asks.
- Graphs cannot be saved yet ([MPA-NOT-2](../../spec/mpa/#mpa-not-2)).
