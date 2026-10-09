#[derive(mabat::View)]
#[view(table = "category")]
struct Category {
    id: i64,
    #[view(child(fk = "parent_id", depth = 3))]
    children: Vec<Box<Category>>,
}

fn main() {}
