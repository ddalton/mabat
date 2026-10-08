#[derive(refract::View)]
#[view(table = "song")]
struct Song {
    title: String,
}

#[derive(refract::View)]
#[view(table = "playlist")]
struct Playlist {
    #[view(child(through = "playlist_song", fk = "playlist_id"))]
    songs: Vec<Song>,
}

fn main() {}
