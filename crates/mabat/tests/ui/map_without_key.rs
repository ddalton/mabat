use std::collections::BTreeMap;

#[derive(mabat::View)]
#[view(table = "setting")]
struct Setting {
    value: String,
}

#[derive(mabat::View)]
#[view(table = "playlist")]
struct Playlist {
    #[view(child(fk = "playlist_id"))]
    settings: BTreeMap<String, Setting>,
}

fn main() {}
