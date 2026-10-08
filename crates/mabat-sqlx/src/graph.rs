//! Graphs: entities in arenas, with typed references between them.
//!
//! A view whose fields are `Ref<T>`, `Option<Ref<T>>` or `Vec<Ref<T>>` is loaded as a
//! [`Graph`] with [`Load::graph`](crate::Load::graph). Each entity is decoded once and stored
//! in the arena of its type; references are typed indices, so cycles, back-references and
//! shared entities need no `Rc`, `Weak` or `RefCell`. Navigation borrows the graph:
//!
//! ```ignore
//! let g = mabat::load::<Task>().by_key(id).graph(&mut conn).await?;
//! for child in g.root().unwrap().children(&g) {
//!     assert_eq!(child.parent(&g).map(|p| &p.name), Some(&g.root().unwrap().name));
//! }
//! ```

use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use mabat_core::ViewShape;

use crate::key::Key;
use crate::{Error, View};

/// A reference to an entity of type `T` in a [`Graph`].
///
/// A `Ref` is an index into the graph it was loaded with; using it with another graph
/// panics.
pub struct Ref<T> {
    index: u32,
    graph: u32,
    _type: PhantomData<fn() -> T>,
}

impl<T> Ref<T> {
    /// The position of the entity in the arena of its type, in load order.
    pub fn index(self) -> usize {
        self.index as usize
    }
}

impl<T> Clone for Ref<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Ref<T> {}

impl<T> PartialEq for Ref<T> {
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index && self.graph == other.graph
    }
}

impl<T> Eq for Ref<T> {}

impl<T> Hash for Ref<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.index.hash(state);
        self.graph.hash(state);
    }
}

impl<T> fmt::Debug for Ref<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = std::any::type_name::<T>().rsplit("::").next().unwrap_or("?");
        write!(f, "Ref<{name}>({})", self.index)
    }
}

/// The address of a view's static shape, which identifies the view type.
pub(crate) fn shape_id(shape: &'static ViewShape) -> usize {
    std::ptr::from_ref(shape) as usize
}

fn next_graph_id() -> u32 {
    static NEXT: AtomicU32 = AtomicU32::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// The arenas of a graph: `Vec<T>` by the shape of `T`.
pub(crate) type Arenas = HashMap<usize, Box<dyn Any + Send + Sync>>;

/// Entities of several types in arenas, with the root entities of a load of `R`.
///
/// `Graph` is `Send + Sync`; share it with `Arc<Graph<R>>`. Change entities with
/// [`Graph::get_mut`], add them with [`Graph::insert`], and save them all with
/// [`save_graph`](crate::save_graph).
pub struct Graph<R> {
    id: u32,
    arenas: Arenas,
    roots: Vec<Ref<R>>,
}

impl<R> Default for Graph<R> {
    fn default() -> Self {
        Graph::new()
    }
}

impl<R: View> Graph<R> {
    /// The root entities, in the order of the root query.
    pub fn roots(&self) -> impl Iterator<Item = &R> {
        self.roots.iter().map(|r| self.get(*r))
    }

    /// The first root entity.
    pub fn root(&self) -> Option<&R> {
        self.roots.first().map(|r| self.get(*r))
    }

    /// References to the root entities.
    pub fn root_refs(&self) -> &[Ref<R>] {
        &self.roots
    }
}

impl<R> Graph<R> {
    /// An empty graph, to build new entities in and save them with
    /// [`save_graph`](crate::save_graph).
    pub fn new() -> Graph<R> {
        Graph { id: next_graph_id(), arenas: HashMap::new(), roots: Vec::new() }
    }

    /// Add an entity to the graph, and get a reference to it, to refer to it from other
    /// entities. A new entity whose key the database generates has the key `None`.
    pub fn insert<T: View>(&mut self, value: T) -> Ref<T> {
        let arena = self
            .arenas
            .entry(shape_id(T::shape()))
            .or_insert_with(|| Box::new(Vec::<T>::new()))
            .downcast_mut::<Vec<T>>()
            .expect("arenas are stored by their type");
        arena.push(value);
        Ref {
            index: u32::try_from(arena.len() - 1).expect("fewer than 2^32 entities"),
            graph: self.id,
            _type: PhantomData,
        }
    }

    /// Make an entity of the graph one of its roots.
    pub fn add_root(&mut self, root: Ref<R>) {
        self.check(root);
        self.roots.push(root);
    }

    /// The arenas, for saving.
    pub(crate) fn arenas_mut(&mut self) -> &mut Arenas {
        &mut self.arenas
    }

    /// The entity a reference points to.
    pub fn get<T: View>(&self, r: Ref<T>) -> &T {
        self.check(r);
        &self.arena::<T>().expect("a reference points to an arena of the graph")[r.index as usize]
    }

    /// The entity a reference points to, to change it.
    pub fn get_mut<T: View>(&mut self, r: Ref<T>) -> &mut T {
        self.check(r);
        let arena = self.arenas.get_mut(&shape_id(T::shape())).and_then(|a| a.downcast_mut::<Vec<T>>());
        &mut arena.expect("a reference points to an arena of the graph")[r.index as usize]
    }

    /// All entities of a type, with references to them.
    pub fn all<T: View>(&self) -> impl Iterator<Item = (Ref<T>, &T)> {
        let id = self.id;
        self.arena::<T>().into_iter().flatten().enumerate().map(move |(i, value)| {
            (Ref { index: u32::try_from(i).expect("fewer than 2^32 entities"), graph: id, _type: PhantomData }, value)
        })
    }

    /// The number of entities of a type.
    pub fn count<T: View>(&self) -> usize {
        self.arena::<T>().map_or(0, Vec::len)
    }

    fn arena<T: View>(&self) -> Option<&Vec<T>> {
        self.arenas.get(&shape_id(T::shape())).and_then(|a| a.downcast_ref::<Vec<T>>())
    }

    fn check<T>(&self, r: Ref<T>) {
        assert_eq!(r.graph, self.id, "a Ref was used with another graph than the one it was loaded with");
    }
}

impl<R> fmt::Debug for Graph<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Graph").field("id", &self.id).field("types", &self.arenas.len()).finish()
    }
}

/// The state shared by the queries and decoders of one load: the entities seen, for graphs
/// and shared (`Arc`) values.
pub(crate) struct Identity {
    graph_id: u32,
    /// The load builds a graph: references are filtered so that no entity is fetched and no
    /// relationship is expanded twice.
    pub(crate) graph: bool,
    state: Mutex<IdentityState>,
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Identity").field("graph_id", &self.graph_id).field("graph", &self.graph).finish()
    }
}

#[derive(Default)]
struct IdentityState {
    /// Entities fetched, by view shape and key.
    fetched: HashSet<(usize, Key)>,
    /// Collections expanded, by view shape, field index and parent key.
    expanded: HashSet<(usize, usize, Key)>,
    /// The keys of the elements of graph collections, by view shape, field index and
    /// parent key, in list order.
    edges: HashMap<(usize, usize, Key), Vec<Key>>,
    /// Arena indices by view shape and key.
    indices: HashMap<(usize, Key), u32>,
    /// The next arena index by view shape.
    counts: HashMap<usize, u32>,
    /// Shared values by view shape and key.
    shared: HashMap<(usize, Key), Arc<dyn Any + Send + Sync>>,
}

impl Identity {
    pub(crate) fn new(graph: bool) -> Arc<Identity> {
        Arc::new(Identity { graph_id: next_graph_id(), graph, state: Mutex::default() })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, IdentityState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Record the entities of the rows of a query as fetched. Returns for each row whether
    /// its entity is fetched for the first time; later rows of an entity are duplicates.
    pub(crate) fn fetch(&self, shape: &'static ViewShape, keys: Vec<Option<Key>>) -> Vec<bool> {
        let id = shape_id(shape);
        let mut state = self.state();
        keys.into_iter().map(|key| key.is_some_and(|key| state.fetched.insert((id, key)))).collect()
    }

    /// Keep the keys of entities that were not fetched yet.
    pub(crate) fn not_fetched(&self, shape: &'static ViewShape, keys: &mut Vec<Key>) {
        let id = shape_id(shape);
        let state = self.state();
        keys.retain(|key| !state.fetched.contains(&(id, key.clone())));
    }

    /// Keep the parent keys whose collection was not expanded yet, and mark them expanded.
    pub(crate) fn expand(&self, shape: &'static ViewShape, field_index: usize, keys: &mut Vec<Key>) {
        let id = shape_id(shape);
        let mut state = self.state();
        keys.retain(|key| state.expanded.insert((id, field_index, key.clone())));
    }

    /// Record the elements of a graph collection of a parent, if not recorded yet.
    pub(crate) fn edges(&self, shape: &'static ViewShape, field_index: usize, parent: Key, elements: Vec<Key>) {
        self.state().edges.entry((shape_id(shape), field_index, parent)).or_insert(elements);
    }

    /// A reference to the entity of type `T` with the key, allocating its arena index.
    pub(crate) fn reference<T: View>(&self, key: Key) -> Ref<T> {
        let id = shape_id(T::shape());
        let mut state = self.state();
        let state = &mut *state;
        let index = *state.indices.entry((id, key)).or_insert_with(|| {
            let count = state.counts.entry(id).or_insert(0);
            *count += 1;
            *count - 1
        });
        Ref { index, graph: self.graph_id, _type: PhantomData }
    }

    /// References to the elements of a graph collection of a parent.
    pub(crate) fn references<T: View>(
        &self,
        shape: &'static ViewShape,
        field_index: usize,
        parent: &Key,
    ) -> Vec<Ref<T>> {
        let keys = self.state().edges.get(&(shape_id(shape), field_index, parent.clone())).cloned().unwrap_or_default();
        keys.into_iter().map(|key| self.reference::<T>(key)).collect()
    }

    /// The shared value of the entity of type `T` with the key, decoded by `decode` the
    /// first time.
    pub(crate) fn shared<T: View>(&self, key: Key, decode: impl FnOnce() -> Result<T, Error>) -> Result<Arc<T>, Error> {
        let id = shape_id(T::shape());
        if let Some(value) = self.state().shared.get(&(id, key.clone())) {
            return Ok(value.clone().downcast::<T>().expect("shared values are stored by their type"));
        }
        // Decode without the lock: decoding can share other values
        let value = Arc::new(decode()?);
        let stored = self.state().shared.entry((id, key)).or_insert_with(|| value.clone()).clone();
        Ok(stored.downcast::<T>().expect("shared values are stored by their type"))
    }

    /// The number of arena indices allocated for each type, to check that every entity
    /// was decoded.
    fn counts(&self) -> HashMap<usize, u32> {
        self.state().counts.clone()
    }
}

/// An arena being filled: `Vec<Option<T>>`, with the function that turns it into the
/// `Vec<T>` of a graph.
struct Building {
    values: Box<dyn Any + Send + Sync>,
    finish: FinishFn,
}

type FinishFn = fn(Box<dyn Any + Send + Sync>, usize) -> Result<Box<dyn Any + Send + Sync>, &'static str>;

/// Unwrap the `count` entities of an arena; the error is the name of the view of an entity
/// that was referenced but not loaded.
fn finish<T: View>(
    values: Box<dyn Any + Send + Sync>,
    count: usize,
) -> Result<Box<dyn Any + Send + Sync>, &'static str> {
    let mut values = *values.downcast::<Vec<Option<T>>>().expect("arenas are stored by their type");
    values.resize_with(count, || None);
    let values: Option<Vec<T>> = values.into_iter().collect();
    match values {
        Some(values) => Ok(Box::new(values)),
        None => Err(T::shape().name),
    }
}

/// Collects the entities of a graph while decoding.
#[doc(hidden)]
pub struct GraphBuilder {
    identity: Arc<Identity>,
    arenas: HashMap<usize, Building>,
}

impl GraphBuilder {
    pub(crate) fn new(identity: Arc<Identity>) -> GraphBuilder {
        GraphBuilder { identity, arenas: HashMap::new() }
    }

    /// Decode the entity with the key with `decode`, unless it is decoded already.
    pub fn store<T: View>(&mut self, key: Key, decode: impl FnOnce() -> Result<T, Error>) -> Result<(), Error> {
        let index = self.identity.reference::<T>(key).index as usize;
        let building = self
            .arenas
            .entry(shape_id(T::shape()))
            .or_insert_with(|| Building { values: Box::new(Vec::<Option<T>>::new()), finish: finish::<T> });
        let arena = building.values.downcast_mut::<Vec<Option<T>>>().expect("arenas are stored by their type");
        if arena.len() <= index {
            arena.resize_with(index + 1, || None);
        }
        if arena[index].is_none() {
            arena[index] = Some(decode()?);
        }
        Ok(())
    }

    /// The graph, with the root entities of the keys. Fails if a reference points to an
    /// entity that was not loaded.
    pub(crate) fn finish<R: View>(mut self, roots: Vec<Key>) -> Result<Graph<R>, Error> {
        let roots = roots.into_iter().map(|key| self.identity.reference::<R>(key)).collect();
        let mut arenas: HashMap<usize, Box<dyn Any + Send + Sync>> = HashMap::new();
        for (id, count) in self.identity.counts() {
            let Some(building) = self.arenas.remove(&id) else {
                return Err(Error::UnloadedReference { view: "an unknown view" });
            };
            let arena =
                (building.finish)(building.values, count as usize).map_err(|view| Error::UnloadedReference { view })?;
            arenas.insert(id, arena);
        }
        Ok(Graph { id: self.identity.graph_id, arenas, roots })
    }
}
