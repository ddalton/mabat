#[derive(mabat::View)]
#[view(table = "task", databases = "postgres, oracle")]
struct Task {
    id: i64,
}

fn main() {}
