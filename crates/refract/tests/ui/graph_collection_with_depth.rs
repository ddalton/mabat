#[derive(refract::View)]
#[view(table = "task")]
struct Task {
    #[view(child(fk = "parent_id", depth = 3))]
    children: Vec<refract::Ref<Task>>,
}

fn main() {}
