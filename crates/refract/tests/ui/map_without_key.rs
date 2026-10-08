use std::collections::BTreeMap;

#[derive(refract::View)]
#[view(table = "setting")]
struct Setting {
    value: String,
}

#[derive(refract::View)]
#[view(table = "playlist")]
struct Playlist {
    #[view(child(fk = "playlist_id"))]
    settings: BTreeMap<String, Setting>,
}

fn main() {}
