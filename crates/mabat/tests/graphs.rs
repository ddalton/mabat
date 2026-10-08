//! Graphs with cycles (`Ref<T>`) and shared values (`Arc<T>`), without `Rc` or `RefCell`.

mod common;

use std::sync::Arc;

use common::TestDb;
use mabat::{Error, Graph, Mabat, Ref, View};

const SCHEMA: &str = r#"
CREATE TABLE team (
    id      BIGINT PRIMARY KEY,
    name    TEXT NOT NULL,
    lead_id BIGINT
);
CREATE TABLE employee (
    id         BIGINT PRIMARY KEY,
    name       TEXT NOT NULL,
    manager_id BIGINT REFERENCES employee (id),
    team_id    BIGINT NOT NULL REFERENCES team (id)
);
ALTER TABLE team ADD FOREIGN KEY (lead_id) REFERENCES employee (id) DEFERRABLE INITIALLY DEFERRED;
CREATE TABLE project (
    id   BIGINT PRIMARY KEY,
    name TEXT NOT NULL
);
CREATE TABLE project_member (
    project_id  BIGINT NOT NULL REFERENCES project (id),
    employee_id BIGINT NOT NULL REFERENCES employee (id),
    PRIMARY KEY (project_id, employee_id)
);
CREATE TABLE note (
    id          SERIAL PRIMARY KEY,
    employee_id BIGINT NOT NULL REFERENCES employee (id),
    author_id   BIGINT NOT NULL REFERENCES employee (id),
    body        TEXT NOT NULL
);

BEGIN;
INSERT INTO team (id, name, lead_id) VALUES (10, 'Engineering', 1), (20, 'Operations', 3);
-- ada manages bob and cy; bob manages dee
INSERT INTO employee (id, name, manager_id, team_id) VALUES
    (1, 'ada', NULL, 10), (2, 'bob', 1, 10), (3, 'cy', 1, 20), (4, 'dee', 2, 10);
COMMIT;
INSERT INTO project (id, name) VALUES (100, 'apollo'), (200, 'zeus');
INSERT INTO project_member (project_id, employee_id) VALUES (100, 1), (100, 4), (200, 1), (200, 2);
INSERT INTO note (employee_id, author_id, body) VALUES (4, 2, 'great first month');
"#;

/// An employee in the org graph: the manager and the reports form cycles, and so do teams
/// and projects with their members.
#[derive(View, Debug)]
#[view(table = "employee")]
pub struct Employee {
    pub name: String,
    #[view(to_one(fk = "manager_id"))]
    pub manager: Option<Ref<Employee>>,
    #[view(child(fk = "manager_id", order_by = "name"))]
    pub reports: Vec<Ref<Employee>>,
    #[view(to_one(fk = "team_id"))]
    pub team: Ref<Team>,
    #[view(child(through = "project_member", fk = "employee_id", target = "project_id", order_by = "name"))]
    pub projects: Vec<Ref<Project>>,
    /// Owned values inside an entity, which reference entities themselves
    #[view(child(fk = "employee_id", order_by = "id"))]
    pub notes: Vec<Note>,
}

#[derive(View, Debug)]
#[view(table = "team")]
pub struct Team {
    pub name: String,
    #[view(to_one(fk = "lead_id"))]
    pub lead: Ref<Employee>,
    #[view(child(fk = "team_id", order_by = "name"))]
    pub members: Vec<Ref<Employee>>,
}

#[derive(View, Debug)]
#[view(table = "project")]
pub struct Project {
    pub name: String,
    #[view(child(through = "project_member", fk = "project_id", target = "employee_id", order_by = "name"))]
    pub members: Vec<Ref<Employee>>,
}

#[derive(View, Debug)]
#[view(table = "note")]
pub struct Note {
    pub body: String,
    #[view(to_one(fk = "author_id"))]
    pub author: Ref<Employee>,
}

async fn setup(name: &str) -> Option<TestDb> {
    TestDb::new(name, SCHEMA).await
}

fn names<'g>(people: impl Iterator<Item = &'g Employee>) -> Vec<&'g str> {
    people.map(|e| e.name.as_str()).collect()
}

fn by_name<'g, R>(graph: &'g Graph<R>, name: &str) -> (Ref<Employee>, &'g Employee) {
    graph.all::<Employee>().find(|(_, e)| e.name == name).expect("the employee is in the graph")
}

#[tokio::test]
async fn cycles_are_followed_both_ways() {
    let Some(mut db) = setup("graph_cycles").await else { return };

    let graph = mabat::load::<Employee>().by_key(4_i64).graph(&mut db.conn).await.unwrap();

    // Everything reachable from dee is loaded once
    assert_eq!(graph.count::<Employee>(), 4);
    assert_eq!(graph.count::<Team>(), 2);
    assert_eq!(graph.count::<Project>(), 2);

    let dee = graph.root().unwrap();
    assert_eq!(dee.name, "dee");
    let bob = dee.manager(&graph).unwrap();
    let ada = bob.manager(&graph).unwrap();
    assert_eq!((bob.name.as_str(), ada.name.as_str()), ("bob", "ada"));
    assert!(ada.manager(&graph).is_none());

    // Following the reports leads back to the same entities: references are equal
    let (ada_ref, _) = by_name(&graph, "ada");
    assert_eq!(names(ada.reports(&graph)), ["bob", "cy"]);
    for report in ada.reports(&graph) {
        assert_eq!(report.manager, Some(ada_ref));
    }
    assert_eq!(names(bob.reports(&graph)), ["dee"]);
    assert_eq!(bob.reports[0], graph.root_refs()[0]);

    // Teams and their members
    let engineering = dee.team(&graph);
    assert_eq!(engineering.name, "Engineering");
    assert_eq!(engineering.lead(&graph).name, "ada");
    assert_eq!(names(engineering.members(&graph)), ["ada", "bob", "dee"]);
    assert_eq!(dee.team, ada.team);
    let cy = by_name(&graph, "cy").1;
    assert_eq!(cy.team(&graph).lead, cy.team(&graph).members[0]);

    // Many-to-many in both directions
    let projects: Vec<&str> = ada.projects(&graph).map(|p| p.name.as_str()).collect();
    assert_eq!(projects, ["apollo", "zeus"]);
    for project in ada.projects(&graph) {
        assert!(project.members.contains(&ada_ref));
    }
    let apollo = ada.projects(&graph).next().unwrap();
    assert_eq!(names(apollo.members(&graph)), ["ada", "dee"]);

    // Owned values inside entities reference entities too
    assert_eq!(dee.notes.len(), 1);
    assert_eq!(dee.notes[0].author(&graph).name, "bob");

    db.drop().await;
}

#[tokio::test]
async fn graphs_of_several_roots_and_changes() {
    let Some(mut db) = setup("graph_roots").await else { return };

    let mut graph = mabat::load::<Team>().order_by("name").graph(&mut db.conn).await.unwrap();
    let teams: Vec<&str> = graph.roots().map(|t| t.name.as_str()).collect();
    assert_eq!(teams, ["Engineering", "Operations"]);
    assert_eq!(graph.count::<Employee>(), 4);

    // Changes go through &mut Graph
    let lead = graph.roots().next().unwrap().lead;
    graph.get_mut(lead).name = "Ada Lovelace".into();
    assert_eq!(graph.roots().next().unwrap().lead(&graph).name, "Ada Lovelace");

    // An empty load is an empty graph
    let empty = mabat::load::<Team>().by_keys(Vec::<i64>::new()).graph(&mut db.conn).await.unwrap();
    assert!(empty.root().is_none());

    db.drop().await;
}

#[tokio::test]
async fn graph_views_need_to_be_loaded_as_graphs() {
    let Some(mut db) = setup("graph_required").await else { return };
    let err = mabat::load::<Employee>().all(&mut db.conn).await.unwrap_err();
    assert!(matches!(err, Error::GraphRequired { view: "Employee" }), "{err}");
    db.drop().await;
}

#[tokio::test]
async fn overrides_apply_to_graph_loads() {
    let Some(mut db) = setup("graph_overrides").await else { return };
    let overrides = r#"
        [query.reports]
        sql = '''
        SELECT e.id AS "$key", e.manager_id AS "$parent", e.name AS "name", e.manager_id AS "$ref.manager",
               e.team_id AS "$ref.team"
        FROM employee e WHERE e.manager_id = ANY($1) ORDER BY e.name DESC
        '''
    "#;
    let mabat = Mabat::builder().register::<Employee>().overrides("Employee", overrides).build(&mut db.conn).await;
    let mabat = mabat.unwrap();
    assert!(mabat.report().diagnostics().is_empty(), "{}", mabat.report());

    let graph = mabat.load::<Employee>().by_key(1_i64).graph(&mut db.conn).await.unwrap();
    assert_eq!(names(graph.root().unwrap().reports(&graph)), ["cy", "bob"]);
    assert_eq!(graph.count::<Employee>(), 4);

    db.drop().await;
}

#[test]
#[should_panic(expected = "another graph")]
fn references_belong_to_their_graph() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let Some((one, two)) = rt.block_on(async {
        let mut db = setup("graph_ownership").await?;
        let one = mabat::load::<Team>().by_key(10_i64).graph(&mut db.conn).await.unwrap();
        let two = mabat::load::<Team>().by_key(10_i64).graph(&mut db.conn).await.unwrap();
        db.drop().await;
        Some((one, two))
    }) else {
        panic!("skipped: another graph");
    };
    let lead = one.root().unwrap().lead;
    two.get(lead);
}

// Shared values: one allocation per entity, in an ordinary tree load

#[derive(View, Debug)]
#[view(table = "employee")]
struct EmployeeCard {
    #[allow(dead_code)]
    name: String,
    #[view(to_one(fk = "team_id"))]
    team: Arc<TeamCard>,
    #[view(to_one(fk = "manager_id"))]
    manager: Option<Arc<Person>>,
}

#[derive(View, Debug)]
#[view(table = "team")]
struct TeamCard {
    #[allow(dead_code)]
    name: String,
}

#[derive(View, Debug, PartialEq)]
#[view(table = "employee")]
struct Person {
    name: String,
}

#[derive(View, Debug)]
#[view(table = "project")]
struct ProjectCard {
    #[allow(dead_code)]
    name: String,
    #[view(child(through = "project_member", fk = "project_id", target = "employee_id", order_by = "name"))]
    members: Vec<Arc<Person>>,
}

#[tokio::test]
async fn shared_values_are_allocated_once() {
    let Some(mut db) = setup("graph_shared").await else { return };

    let cards = mabat::load::<EmployeeCard>().order_by("id").all(&mut db.conn).await.unwrap();
    let [ada, bob, cy, dee] = cards.as_slice() else { panic!("{cards:?}") };
    assert!(Arc::ptr_eq(&ada.team, &bob.team));
    assert!(Arc::ptr_eq(&ada.team, &dee.team));
    assert!(!Arc::ptr_eq(&ada.team, &cy.team));
    assert!(Arc::ptr_eq(bob.manager.as_ref().unwrap(), cy.manager.as_ref().unwrap()));
    assert_eq!(dee.manager.as_deref(), Some(&Person { name: "bob".into() }));

    // Shared across collections: ada is on both projects, as one value
    let projects = mabat::load::<ProjectCard>().order_by("name").all(&mut db.conn).await.unwrap();
    assert!(Arc::ptr_eq(&projects[0].members[0], &projects[1].members[0]));
    assert_eq!(projects[0].members[0].name, "ada");

    // Shared values are per load
    let again = mabat::load::<EmployeeCard>().by_key(1_i64).one(&mut db.conn).await.unwrap();
    assert!(!Arc::ptr_eq(&again.team, &ada.team));

    db.drop().await;
}
