//! Saving graphs on every database: new entities with generated keys referencing each other,
//! cycles of references, collections that are the inverse of a reference, collections that
//! write their elements' foreign key, link tables, and references from owned values.

use mabat::{Conn, Error, Graph, Ref, View};
use sqlx::{AssertSqlSafe, Connection, Executor};

/// The schema, with `{key}` the declaration of a generated 64 bit key. Foreign keys are
/// declared at the table level, which MySQL enforces.
const SCHEMA: &str = r#"
CREATE TABLE team (id {key}, name VARCHAR(100) NOT NULL);
CREATE TABLE employee (
    id {key}, name VARCHAR(100) NOT NULL, team_id BIGINT, manager_id BIGINT,
    FOREIGN KEY (team_id) REFERENCES team (id), FOREIGN KEY (manager_id) REFERENCES employee (id)
);
CREATE TABLE skill (id BIGINT PRIMARY KEY, name VARCHAR(100) NOT NULL);
CREATE TABLE team_skill (
    team_id BIGINT NOT NULL, skill_id BIGINT NOT NULL, PRIMARY KEY (team_id, skill_id),
    FOREIGN KEY (team_id) REFERENCES team (id), FOREIGN KEY (skill_id) REFERENCES skill (id)
);
CREATE TABLE team_note (
    id {key}, team_id BIGINT NOT NULL, body VARCHAR(100) NOT NULL, author_id BIGINT NOT NULL,
    FOREIGN KEY (team_id) REFERENCES team (id), FOREIGN KEY (author_id) REFERENCES employee (id)
);
CREATE TABLE project (id {key}, name VARCHAR(100) NOT NULL);
CREATE TABLE task (
    id {key}, project_id BIGINT, position INT, title VARCHAR(100) NOT NULL,
    FOREIGN KEY (project_id) REFERENCES project (id)
);
"#;

#[derive(View, Debug)]
#[view(table = "team")]
struct Team {
    #[view(generated)]
    id: Option<i64>,
    name: String,
    /// The inverse of `Employee::team`: written by the employees
    #[view(child(fk = "team_id", order_by = "id"))]
    members: Vec<Ref<Employee>>,
    #[view(child(through = "team_skill", fk = "team_id", target = "skill_id", order_by = "id"))]
    skills: Vec<Ref<Skill>>,
    /// Owned, and referencing employees: the team is saved after its notes' authors
    #[view(child(fk = "team_id", order_by = "id"))]
    notes: Vec<TeamNote>,
}

#[derive(View, Debug)]
#[view(table = "employee")]
struct Employee {
    #[view(generated)]
    id: Option<i64>,
    name: String,
    #[view(to_one(fk = "team_id"))]
    team: Option<Ref<Team>>,
    #[view(to_one(fk = "manager_id"))]
    manager: Option<Ref<Employee>>,
    /// The inverse of `manager`
    #[view(child(fk = "manager_id", order_by = "id"))]
    reports: Vec<Ref<Employee>>,
}

#[derive(View, Debug)]
#[view(table = "skill")]
struct Skill {
    id: i64,
    name: String,
}

#[derive(View, Debug)]
#[view(table = "team_note")]
struct TeamNote {
    #[view(generated)]
    id: Option<i64>,
    body: String,
    #[view(to_one(fk = "author_id"))]
    author: Ref<Employee>,
}

/// A collection whose elements do not reference the project: it writes their `project_id`.
#[derive(View, Debug)]
#[view(table = "project")]
struct Project {
    #[view(generated)]
    id: Option<i64>,
    name: String,
    #[view(child(fk = "project_id", index = "position"))]
    tasks: Vec<Ref<Task>>,
}

#[derive(View, Debug)]
#[view(table = "task")]
struct Task {
    #[view(generated)]
    id: Option<i64>,
    title: String,
}

/// A task's row, with the columns its project writes.
#[derive(View, Debug, PartialEq)]
#[view(table = "task")]
struct TaskRow {
    id: i64,
    title: String,
    project_id: Option<i64>,
    position: Option<i32>,
}

/// The team as text: its members with their managers and reports, skills and notes.
fn describe(g: &Graph<Team>) -> String {
    let team = g.root().unwrap();
    let name = |r: Ref<Employee>| g.get(r).name.clone();
    let members: Vec<String> = team
        .members
        .iter()
        .map(|&m| {
            let e = g.get(m);
            let reports: Vec<String> = e.reports.iter().map(|&r| name(r)).collect();
            let team = e.team.map(|t| g.get(t).name.clone()).unwrap_or_default();
            format!("{} (team {team}, manager {:?}, reports {reports:?})", e.name, e.manager.map(name))
        })
        .collect();
    let skills: Vec<&str> = team.skills.iter().map(|&s| g.get(s).name.as_str()).collect();
    let notes: Vec<String> = team.notes.iter().map(|n| format!("{} by {}", n.body, name(n.author))).collect();
    format!("{}: {members:?} {skills:?} {notes:?}", team.name)
}

fn employee(name: &str) -> Employee {
    Employee { id: None, name: name.into(), team: None, manager: None, reports: Vec::new() }
}

async fn load_team<C: Conn>(conn: &mut C, id: i64) -> Graph<Team>
where
    Team: mabat::ViewDecoder<C::Backend>,
{
    mabat::load::<Team>().by_key(id).graph(conn).await.unwrap()
}

async fn teams<C: Conn>(conn: &mut C)
where
    C::Backend: Send,
    <C::Backend as sqlx::Database>::Connection: Send,
    Team: mabat::ViewEncoder<C::Backend>,
    Employee: mabat::ViewEncoder<C::Backend>,
{
    // A new graph: every key is generated, and an entity is saved after what it references
    let mut g = Graph::<Team>::new();
    let rust = g.insert(Skill { id: 1, name: "rust".into() });
    let sql = g.insert(Skill { id: 2, name: "sql".into() });
    let ada = g.insert(employee("Ada"));
    let grace = g.insert(employee("Grace"));
    let team = g.insert(Team {
        id: None,
        name: "Core".into(),
        members: vec![ada, grace],
        skills: vec![rust, sql],
        notes: vec![TeamNote { id: None, body: "Kickoff".into(), author: ada }],
    });
    g.add_root(team);
    for e in [ada, grace] {
        g.get_mut(e).team = Some(team);
    }
    g.get_mut(grace).manager = Some(ada);
    g.get_mut(ada).reports = vec![grace];
    mabat::save_graph(&mut g, conn).await.unwrap();
    let id = g.get(team).id.expect("the team's key is written back");
    assert!(g.get(ada).id.is_some() && g.get(grace).id.is_some());
    assert!(g.get(team).notes[0].id.is_some());
    let expected = r#"Core: ["Ada (team Core, manager None, reports [\"Grace\"])", "Grace (team Core, manager Some(\"Ada\"), reports [])"] ["rust", "sql"] ["Kickoff by Ada"]"#;
    assert_eq!(describe(&g), expected);
    assert_eq!(describe(&load_team(conn, id).await), expected);

    // A cycle of references: each manages the other
    let mut g = load_team(conn, id).await;
    let members = g.root().unwrap().members.clone();
    let (ada, grace) = (members[0], members[1]);
    g.get_mut(ada).manager = Some(grace);
    g.get_mut(grace).reports = vec![ada];
    g.get_mut(ada).name = "Ada L.".into();
    let skills = g.root().unwrap().skills.clone();
    let team = g.root_refs()[0];
    g.get_mut(team).skills = vec![skills[1]];
    mabat::save_graph(&mut g, conn).await.unwrap();
    let expected = r#"Core: ["Ada L. (team Core, manager Some(\"Grace\"), reports [\"Grace\"])", "Grace (team Core, manager Some(\"Ada L.\"), reports [\"Ada L.\"])"] ["sql"] ["Kickoff by Ada L."]"#;
    assert_eq!(describe(&load_team(conn, id).await), expected);

    // A collection and the reference it is the inverse of must agree
    let mut g = load_team(conn, id).await;
    let grace = g.root().unwrap().members[1];
    g.get_mut(grace).team = None;
    let error = mabat::save_graph(&mut g, conn).await.unwrap_err();
    assert!(error.to_string().contains("is an element of its collection but does not reference it"), "{error}");
    assert_eq!(describe(&load_team(conn, id).await), expected, "nothing was saved");

    // A value with references into a graph is saved with its graph
    let g = load_team(conn, id).await;
    let mut alone = Employee { id: None, name: "Alan".into(), team: g.root_refs().first().copied(), ..employee("") };
    let error = mabat::save(&mut alone, conn).await.unwrap_err();
    assert!(matches!(error, Error::Write { .. }) && error.to_string().contains("save_graph"), "{error}");
}

/// The title, project and position of each task.
async fn task_rows<C: Conn>(conn: &mut C) -> Vec<(String, Option<i64>, Option<i32>)>
where
    TaskRow: mabat::ViewDecoder<C::Backend>,
{
    let rows = mabat::load::<TaskRow>().order_by("id").all(conn).await.unwrap();
    rows.into_iter().map(|r| (r.title, r.project_id, r.position)).collect()
}

async fn projects<C: Conn>(conn: &mut C)
where
    C::Backend: Send,
    <C::Backend as sqlx::Database>::Connection: Send,
    Project: mabat::ViewEncoder<C::Backend>,
    TaskRow: mabat::ViewDecoder<C::Backend>,
{
    // The collection writes its elements' foreign key and position
    let mut g = Graph::<Project>::new();
    let tasks: Vec<Ref<Task>> =
        ["Plan", "Build", "Ship"].iter().map(|t| g.insert(Task { id: None, title: (*t).into() })).collect();
    let alpha = g.insert(Project { id: None, name: "alpha".into(), tasks: tasks.clone() });
    let beta = g.insert(Project { id: None, name: "beta".into(), tasks: Vec::new() });
    g.add_root(alpha);
    g.add_root(beta);
    mabat::save_graph(&mut g, conn).await.unwrap();
    let (alpha_id, beta_id) = (g.get(alpha).id.unwrap(), g.get(beta).id.unwrap());
    let p = |id| Some(id);
    assert_eq!(
        task_rows(conn).await,
        [
            ("Plan".to_string(), p(alpha_id), Some(0)),
            ("Build".to_string(), p(alpha_id), Some(1)),
            ("Ship".to_string(), p(alpha_id), Some(2)),
        ]
    );

    // Moved to another project, and removed: the removed task is unlinked, not deleted
    let mut g = mabat::load::<Project>().order_by("id").graph(conn).await.unwrap();
    let refs = g.root_refs().to_vec();
    let tasks = g.get(refs[0]).tasks.clone();
    g.get_mut(refs[0]).tasks = vec![tasks[2]];
    g.get_mut(refs[1]).tasks = vec![tasks[1]];
    mabat::save_graph(&mut g, conn).await.unwrap();
    assert_eq!(
        task_rows(conn).await,
        [
            ("Plan".to_string(), None, Some(0)),
            ("Build".to_string(), p(beta_id), Some(0)),
            ("Ship".to_string(), p(alpha_id), Some(0)),
        ]
    );
    let g = mabat::load::<Project>().by_key(beta_id).graph(conn).await.unwrap();
    let titles: Vec<&str> = g.root().unwrap().tasks.iter().map(|&t| g.get(t).title.as_str()).collect();
    assert_eq!(titles, ["Build"]);
}

async fn scenario<C: Conn>(conn: &mut C)
where
    C::Backend: Send,
    <C::Backend as sqlx::Database>::Connection: Send,
    Team: mabat::ViewEncoder<C::Backend>,
    Employee: mabat::ViewEncoder<C::Backend>,
    Project: mabat::ViewEncoder<C::Backend>,
    TaskRow: mabat::ViewDecoder<C::Backend>,
{
    teams(conn).await;
    projects(conn).await;
}

#[tokio::test]
async fn sqlite() {
    let mut conn = sqlx::SqliteConnection::connect("sqlite::memory:").await.unwrap();
    conn.execute("PRAGMA foreign_keys = ON").await.unwrap();
    conn.execute(AssertSqlSafe(SCHEMA.replace("{key}", "INTEGER PRIMARY KEY"))).await.unwrap();
    scenario(&mut conn).await;
}

#[tokio::test]
async fn postgres() {
    let Ok(url) = std::env::var("MABAT_TEST_DATABASE_URL") else {
        eprintln!("skipping: MABAT_TEST_DATABASE_URL is not set (see scripts/with-postgres.sh)");
        return;
    };
    let mut conn = sqlx::PgConnection::connect(&url).await.unwrap();
    let schema = format!("mabat_graph_save_{}", std::process::id());
    let setup = format!(
        "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE; CREATE SCHEMA \"{schema}\"; SET search_path TO \"{schema}\""
    );
    conn.execute(AssertSqlSafe(setup)).await.unwrap();
    let ddl = SCHEMA.replace("{key}", "BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY");
    conn.execute(AssertSqlSafe(ddl)).await.unwrap();
    scenario(&mut conn).await;
    conn.execute(AssertSqlSafe(format!("DROP SCHEMA \"{schema}\" CASCADE"))).await.unwrap();
}

#[tokio::test]
async fn mysql() {
    let Ok(url) = std::env::var("MABAT_TEST_MYSQL_URL") else {
        eprintln!("skipping: MABAT_TEST_MYSQL_URL is not set (see scripts/with-mysql.sh)");
        return;
    };
    let mut conn = sqlx::MySqlConnection::connect(&url).await.unwrap();
    let database = format!("mabat_graph_save_{}", std::process::id());
    let setup = format!("DROP DATABASE IF EXISTS `{database}`; CREATE DATABASE `{database}`; USE `{database}`");
    conn.execute(AssertSqlSafe(setup)).await.unwrap();
    let ddl = SCHEMA.replace("{key}", "BIGINT AUTO_INCREMENT PRIMARY KEY");
    conn.execute(AssertSqlSafe(ddl)).await.unwrap();
    scenario(&mut conn).await;
    conn.execute(AssertSqlSafe(format!("DROP DATABASE `{database}`"))).await.unwrap();
}
