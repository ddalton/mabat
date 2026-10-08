#[derive(refract::View)]
#[view(table = "task")]
struct Task {
    #[view(child(order_by = "name"))]
    children: Vec<Task>,
}

fn main() {}
