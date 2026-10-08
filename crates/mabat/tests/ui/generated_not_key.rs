use mabat::View;

#[derive(View)]
#[view(table = "task")]
struct Task {
    id: i64,
    #[view(generated)]
    number: Option<i64>,
}

fn main() {}
