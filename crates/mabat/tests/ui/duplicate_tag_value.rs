#[derive(mabat::View)]
#[view(tag = "kind")]
enum Status {
    #[view(tag_value = "open")]
    Open,
    #[view(tag_value = "open")]
    Reopened,
}

fn main() {}
