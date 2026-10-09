#[derive(mabat::View)]
#[view(table = "category")]
struct Category {
    id: i64,
    #[view(to_one(fk = "parent_id", recursive = "cte", depth = 3))]
    parent: Option<Box<Category>>,
}

fn main() {}
