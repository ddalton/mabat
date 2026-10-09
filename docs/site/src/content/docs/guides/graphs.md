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

## Saving a graph

`mabat::save_graph` saves every entity of a graph in one transaction, each as `save` saves a value, with its `Ref`
fields written as the keys of the entities they point to ([MPA-WRITE-14](../../spec/mpa/#mpa-write-14)). Build
new entities in a graph with `Graph::insert`, which returns a `Ref` to use in other entities:

```rust
let mut graph = Graph::<Employee>::new();
let team = graph.insert(Team { id: None, name: "Core".into(), members: vec![] });
let ada = graph.insert(Employee { id: None, name: "Ada".into(), team, manager: None, reports: vec![] });
let grace = graph.insert(Employee { id: None, name: "Grace".into(), team, manager: Some(ada), reports: vec![] });
graph.get_mut(ada).reports = vec![grace];
graph.get_mut(team).members = vec![ada, grace];
graph.add_root(ada);
mabat::save_graph(&mut graph, &mut tx).await?;   // the generated keys are written back
```

- **Order.** An entity is saved after the entities it references, so their keys exist, generated or not, and
  foreign keys hold as rows are inserted. In a cycle, the optional references are written NULL first and set once
  the cycle is saved; required references in a cycle fail with `Error::Write`
  ([MPA-WRITE-15](../../spec/mpa/#mpa-write-15)).
- **Collections by a foreign key.** `reports` above is the inverse of `manager`: the same `manager_id` column.
  The reference is what is written, and the collection must agree with it. A collection whose elements have no
  such reference writes their foreign key itself, and sets it to NULL for rows it no longer holds
  ([MPA-WRITE-16](../../spec/mpa/#mpa-write-16)).
- **Link tables.** A collection through a link table replaces the entity's links
  ([MPA-WRITE-17](../../spec/mpa/#mpa-write-17)).
- **Everything is written.** `save_graph` writes every entity of the graph, changed or not. `save` and
  `save_changes` refuse a value with `Ref` fields.

## Saving what changed

A graph records the entities added with `insert` and those handed out by `get_mut`, whether or not they were then
changed. `mabat::save_graph_changes` writes only those, in the same order and with the same checks as `save_graph`
([MPA-WRITE-20](../../spec/mpa/#mpa-write-20)):

```rust
let mut graph = mabat::load::<Employee>().by_key(id).graph(&mut tx).await?;
let ada = graph.root_refs()[0];
graph.get_mut(ada).name = "Ada L.".into();
assert!(graph.is_changed(ada));
mabat::save_graph_changes(&mut graph, &mut tx).await?;   // writes Ada's row only
assert!(!graph.is_changed(ada));
```

- **Other entities' rows are kept.** Their keys are used as loaded. The exception is a changed entity's
  collection by a foreign key, which writes that key to its elements' rows.
- **Collections of changed entities only.** Link rows are replaced, and rows no longer in a collection unlinked,
  for the changed entities' collections.
- **The graph must still agree.** A collection that is the inverse of a reference is checked against it across
  the whole graph, so changing one side means changing the other too, as with `save_graph`.
- A successful save leaves no entity changed; a failed one rolls back and keeps them changed, to save again.
