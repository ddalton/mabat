struct NotAColumn;

#[derive(mabat::View)]
#[view(table = "task", databases = "postgres")]
struct Task {
    value: NotAColumn,
}

fn main() {}
