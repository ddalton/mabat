#[derive(mabat::View)]
#[view(tag = "kind")]
enum Status {
    Open,
    Blocked(String),
}

fn main() {}
