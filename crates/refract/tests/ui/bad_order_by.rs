#[derive(refract::View)]
#[view(table = "task")]
struct Task {
    #[view(child(fk = "parent_id", order_by = "name sideways"))]
    children: Vec<Task>,
}

fn main() {}
