#[derive(mabat::View)]
#[view(embedded)]
struct Tagged<T> {
    #[view(json)]
    value: T,
}

fn main() {}
