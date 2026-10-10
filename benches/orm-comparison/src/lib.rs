//! The same load with Mabat, hand-written SQLx, SeaORM and Diesel: tasks by key, each with its
//! subtasks in order. Each library loads it its own idiomatic, batched way: one query for the
//! tasks and one for all of their subtasks, grouped by task in Rust.

use std::collections::HashMap;

/// Tasks in the database, each with [`SUBTASKS`] subtasks.
pub const TASKS: i64 = 10_000;
pub const SUBTASKS: i64 = 10;

/// The schema and data, in the default schema of the database.
pub const SETUP: &str = "
DROP TABLE IF EXISTS subtask;
DROP TABLE IF EXISTS task;
CREATE TABLE task (id BIGINT PRIMARY KEY, name TEXT NOT NULL, description TEXT);
CREATE TABLE subtask (
    id BIGINT PRIMARY KEY, task_id BIGINT NOT NULL REFERENCES task (id), name TEXT NOT NULL, position INTEGER NOT NULL
);
CREATE INDEX subtask_task_id ON subtask (task_id);
INSERT INTO task SELECT t, 'Task ' || t, CASE WHEN t % 3 = 0 THEN NULL ELSE 'About task ' || t END
    FROM generate_series(1, 10000) t;
INSERT INTO subtask SELECT t * 10 + s, t, 'Subtask ' || s, 10 - s
    FROM generate_series(1, 10000) t, generate_series(0, 9) s;
ANALYZE task;
ANALYZE subtask;
";

pub mod with_mabat {
    use mabat::View;

    #[derive(View, Debug)]
    #[view(table = "task")]
    pub struct Task {
        pub id: i64,
        pub name: String,
        pub description: Option<String>,
        #[view(child(fk = "task_id", order_by = "position"))]
        pub subtasks: Vec<Subtask>,
    }

    #[derive(View, Debug)]
    #[view(table = "subtask")]
    pub struct Subtask {
        pub id: i64,
        pub name: String,
        pub position: i32,
    }

    pub async fn load(conn: &mut sqlx::PgConnection, keys: &[i64]) -> Vec<Task> {
        mabat::load::<Task>().by_keys(keys.iter().copied()).all(conn).await.unwrap()
    }
}

pub mod with_sqlx {
    use super::HashMap;
    use sqlx::Row;

    #[derive(Debug)]
    pub struct Task {
        pub id: i64,
        pub name: String,
        pub description: Option<String>,
        pub subtasks: Vec<Subtask>,
    }

    #[derive(Debug)]
    pub struct Subtask {
        pub id: i64,
        pub name: String,
        pub position: i32,
    }

    pub async fn load(conn: &mut sqlx::PgConnection, keys: &[i64]) -> Vec<Task> {
        let rows = sqlx::query("SELECT id, name, description FROM task WHERE id = ANY($1)")
            .bind(keys)
            .fetch_all(&mut *conn)
            .await
            .unwrap();
        let ids: Vec<i64> = rows.iter().map(|r| r.get("id")).collect();
        let child_rows =
            sqlx::query("SELECT id, task_id, name, position FROM subtask WHERE task_id = ANY($1) ORDER BY position, id")
                .bind(&ids)
                .fetch_all(&mut *conn)
                .await
                .unwrap();
        let mut children: HashMap<i64, Vec<Subtask>> = HashMap::new();
        for row in &child_rows {
            children.entry(row.get("task_id")).or_default().push(Subtask {
                id: row.get("id"),
                name: row.get("name"),
                position: row.get("position"),
            });
        }
        rows.iter()
            .map(|row| {
                let id: i64 = row.get("id");
                Task {
                    id,
                    name: row.get("name"),
                    description: row.get("description"),
                    subtasks: children.remove(&id).unwrap_or_default(),
                }
            })
            .collect()
    }
}

pub mod with_sea_orm {
    use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, LoaderTrait, QueryFilter, QueryOrder};

    pub mod task {
        use sea_orm::entity::prelude::*;

        #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
        #[sea_orm(table_name = "task")]
        pub struct Model {
            #[sea_orm(primary_key, auto_increment = false)]
            pub id: i64,
            pub name: String,
            pub description: Option<String>,
        }

        #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
        pub enum Relation {
            #[sea_orm(has_many = "super::subtask::Entity")]
            Subtask,
        }

        impl Related<super::subtask::Entity> for Entity {
            fn to() -> RelationDef {
                Relation::Subtask.def()
            }
        }

        impl ActiveModelBehavior for ActiveModel {}
    }

    pub mod subtask {
        use sea_orm::entity::prelude::*;

        #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
        #[sea_orm(table_name = "subtask")]
        pub struct Model {
            #[sea_orm(primary_key, auto_increment = false)]
            pub id: i64,
            pub task_id: i64,
            pub name: String,
            pub position: i32,
        }

        #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
        pub enum Relation {
            #[sea_orm(belongs_to = "super::task::Entity", from = "Column::TaskId", to = "super::task::Column::Id")]
            Task,
        }

        impl Related<super::task::Entity> for Entity {
            fn to() -> RelationDef {
                Relation::Task.def()
            }
        }

        impl ActiveModelBehavior for ActiveModel {}
    }

    pub async fn load(db: &DatabaseConnection, keys: &[i64]) -> Vec<(task::Model, Vec<subtask::Model>)> {
        let tasks = task::Entity::find().filter(task::Column::Id.is_in(keys.iter().copied())).all(db).await.unwrap();
        let subtasks = tasks
            .load_many(subtask::Entity::find().order_by_asc(subtask::Column::Position), db)
            .await
            .unwrap();
        tasks.into_iter().zip(subtasks).collect()
    }
}

pub mod with_diesel {
    use diesel::prelude::*;
    use diesel_async::{AsyncPgConnection, RunQueryDsl};

    diesel::table! {
        task (id) {
            id -> BigInt,
            name -> Text,
            description -> Nullable<Text>,
        }
    }

    diesel::table! {
        subtask (id) {
            id -> BigInt,
            task_id -> BigInt,
            name -> Text,
            position -> Integer,
        }
    }

    diesel::joinable!(subtask -> task (task_id));
    diesel::allow_tables_to_appear_in_same_query!(task, subtask);

    #[derive(Debug, Queryable, Selectable, Identifiable)]
    #[diesel(table_name = task)]
    pub struct Task {
        pub id: i64,
        pub name: String,
        pub description: Option<String>,
    }

    #[derive(Debug, Queryable, Selectable, Identifiable, Associations)]
    #[diesel(table_name = subtask, belongs_to(Task, foreign_key = task_id))]
    pub struct Subtask {
        pub id: i64,
        pub task_id: i64,
        pub name: String,
        pub position: i32,
    }

    pub async fn load(conn: &mut AsyncPgConnection, keys: &[i64]) -> Vec<(Task, Vec<Subtask>)> {
        let tasks: Vec<Task> =
            task::table.filter(task::id.eq_any(keys)).select(Task::as_select()).load(conn).await.unwrap();
        let subtasks: Vec<Subtask> = Subtask::belonging_to(&tasks)
            .order(subtask::position)
            .select(Subtask::as_select())
            .load(conn)
            .await
            .unwrap();
        let grouped = subtasks.grouped_by(&tasks);
        tasks.into_iter().zip(grouped).collect()
    }
}
