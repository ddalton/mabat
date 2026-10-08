#[derive(mabat::View)]
#[view(table = "doc")]
struct Doc {
    id: i64,
    #[view(version, child(fk = "doc_id"))]
    sections: Vec<Section>,
}

#[derive(mabat::View)]
#[view(table = "section")]
struct Section {
    id: i64,
}

fn main() {}
