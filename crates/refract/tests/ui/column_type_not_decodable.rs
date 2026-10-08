struct NotAColumn;

#[derive(refract::View)]
#[view(table = "task")]
struct Task {
    value: NotAColumn,
}

fn main() {}
