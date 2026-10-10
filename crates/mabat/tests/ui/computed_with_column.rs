#[derive(mabat::View)]
#[view(table = "customer")]
struct Report {
    id: i64,
    #[view(computed, column = "total")]
    total: i64,
}

fn main() {}
