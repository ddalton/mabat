#[derive(mabat::View)]
#[view(table = "task")]
struct Task {
    #[view(child(fk = "parent_id", depth = 3))]
    children: Vec<mabat::Ref<Task>>,
}

fn main() {}
