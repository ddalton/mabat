// Only PostgreSQL shares snapshots between connections
fn pooled(pool: &sqlx::MySqlPool) -> mabat::Pooled<sqlx::MySql> {
    mabat::Pooled::snapshot(pool, 4)
}

fn main() {}
