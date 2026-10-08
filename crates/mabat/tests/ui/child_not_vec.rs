#[derive(mabat::View)]
#[view(table = "note")]
struct Note {
    body: String,
}

#[derive(mabat::View)]
#[view(table = "task")]
struct Task {
    #[view(child(fk = "task_id"))]
    note: Option<Note>,
}

fn main() {}
