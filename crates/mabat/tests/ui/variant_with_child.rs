#[derive(mabat::View)]
#[view(table = "note")]
struct Note {
    body: String,
}

#[derive(mabat::View)]
#[view(tag = "kind")]
enum Status {
    Open,
    Commented {
        #[view(child(fk = "task_id"))]
        notes: Vec<Note>,
    },
}

fn main() {}
