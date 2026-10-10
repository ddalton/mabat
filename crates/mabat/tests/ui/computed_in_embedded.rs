#[derive(mabat::View)]
#[view(embedded)]
struct Totals {
    #[view(computed)]
    total: i64,
}

fn main() {}
