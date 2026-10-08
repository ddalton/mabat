#[derive(mabat::View)]
#[view(embedded)]
struct Address {
    #[view(child(fk = "address_id"))]
    lines: Vec<String>,
}

fn main() {}
