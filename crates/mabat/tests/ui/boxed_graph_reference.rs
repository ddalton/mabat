#[derive(mabat::View)]
#[view(table = "category")]
struct Category {
    id: i64,
    #[view(to_one(fk = "parent_id"))]
    parent: Option<Box<mabat::Ref<Category>>>,
}

fn main() {}
