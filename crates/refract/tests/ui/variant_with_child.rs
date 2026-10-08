#[derive(refract::View)]
#[view(table = "note")]
struct Note {
    body: String,
}

#[derive(refract::View)]
#[view(tag = "kind")]
enum Status {
    Open,
    Commented {
        #[view(child(fk = "task_id"))]
        notes: Vec<Note>,
    },
}

fn main() {}
