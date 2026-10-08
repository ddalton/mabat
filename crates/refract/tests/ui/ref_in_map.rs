use std::collections::BTreeMap;

#[derive(refract::View)]
#[view(table = "setting")]
struct Setting {
    value: String,
}

#[derive(refract::View)]
#[view(table = "playlist")]
struct Playlist {
    #[view(child(fk = "playlist_id", key = "name"))]
    settings: BTreeMap<String, refract::Ref<Setting>>,
}

fn main() {}
