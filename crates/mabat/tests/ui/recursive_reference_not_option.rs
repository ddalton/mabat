#[derive(mabat::View)]
#[view(table = "category")]
struct Category {
    id: i64,
    #[view(to_one(fk = "parent_id", depth = 3))]
    parent: Box<Category>,
}

fn main() {}
