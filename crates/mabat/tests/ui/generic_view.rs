#[derive(mabat::View)]
#[view(table = "booking")]
struct Booking<T> {
    id: i64,
    value: T,
}

fn main() {}
