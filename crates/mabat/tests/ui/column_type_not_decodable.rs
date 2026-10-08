struct NotAColumn;

#[derive(mabat::View)]
#[view(table = "task")]
struct Task {
    value: NotAColumn,
}

fn main() {}
