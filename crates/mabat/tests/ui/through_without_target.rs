#[derive(mabat::View)]
#[view(table = "song")]
struct Song {
    title: String,
}

#[derive(mabat::View)]
#[view(table = "playlist")]
struct Playlist {
    #[view(child(through = "playlist_song", fk = "playlist_id"))]
    songs: Vec<Song>,
}

fn main() {}
