use mabat::View;

#[derive(View)]
#[view(table = "task")]
struct Task {
    #[view(generated)]
    id: i64,
    name: String,
}

fn main() {}
