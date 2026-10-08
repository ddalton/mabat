#[derive(mabat::View)]
#[view(tag = "kind", strategy = "table_per_variant")]
enum Payment {
    Cash,
    Card { last4: String },
}

fn main() {}
