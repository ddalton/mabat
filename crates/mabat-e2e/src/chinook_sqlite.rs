//! The views of [`crate::chinook`] on SQLite, where Chinook keeps money as `REAL`: the same
//! views, with `f64` for `Decimal`.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::NaiveDateTime;
use mabat::{Ref, View};

// Invoices with lines, tracks, albums and artists

#[derive(View, Debug, Clone, PartialEq)]
#[view(databases = "sqlite")]
#[view(table = "invoice", key = "invoice_id")]
pub struct InvoiceView {
    pub invoice_id: i32,
    /// `timestamp` without time zone
    pub invoice_date: NaiveDateTime,
    pub total: f64,
    #[view(embed(prefix = "billing_"))]
    pub billing: BillingAddress,
    #[view(to_one(fk = "customer_id"))]
    pub customer: CustomerName,
    #[view(child(fk = "invoice_id", order_by = "invoice_line_id"))]
    pub lines: Vec<InvoiceLine>,
}

#[derive(View, Debug, Clone, PartialEq)]
#[view(databases = "sqlite")]
#[view(embedded)]
pub struct BillingAddress {
    pub address: Option<String>,
    pub city: Option<String>,
    pub state: Option<String>,
    pub country: Option<String>,
    pub postal_code: Option<String>,
}

#[derive(View, Debug, Clone, PartialEq)]
#[view(databases = "sqlite")]
#[view(table = "customer", key = "customer_id")]
pub struct CustomerName {
    pub customer_id: i32,
    pub first_name: String,
    pub last_name: String,
    pub company: Option<String>,
}

#[derive(View, Debug, Clone, PartialEq)]
#[view(databases = "sqlite")]
#[view(table = "invoice_line", key = "invoice_line_id")]
pub struct InvoiceLine {
    pub invoice_line_id: i32,
    pub unit_price: f64,
    pub quantity: i32,
    #[view(to_one(fk = "track_id"))]
    pub track: Arc<TrackView>,
}

#[derive(View, Debug, Clone, PartialEq)]
#[view(databases = "sqlite")]
#[view(table = "track", key = "track_id")]
pub struct TrackView {
    pub track_id: i32,
    pub name: String,
    pub composer: Option<String>,
    pub milliseconds: i32,
    pub unit_price: f64,
    #[view(to_one(fk = "album_id"))]
    pub album: Option<Arc<AlbumView>>,
    #[view(to_one(fk = "genre_id"))]
    pub genre: Option<Arc<GenreView>>,
    #[view(to_one(fk = "media_type_id"))]
    pub media_type: Arc<MediaTypeView>,
}

#[derive(View, Debug, Clone, PartialEq)]
#[view(databases = "sqlite")]
#[view(table = "album", key = "album_id")]
pub struct AlbumView {
    pub album_id: i32,
    pub title: String,
    #[view(to_one(fk = "artist_id"))]
    pub artist: Arc<ArtistName>,
}

#[derive(View, Debug, Clone, PartialEq)]
#[view(databases = "sqlite")]
#[view(table = "artist", key = "artist_id")]
pub struct ArtistName {
    pub artist_id: i32,
    pub name: Option<String>,
}

#[derive(View, Debug, Clone, PartialEq)]
#[view(databases = "sqlite")]
#[view(table = "genre", key = "genre_id")]
pub struct GenreView {
    pub genre_id: i32,
    pub name: Option<String>,
}

#[derive(View, Debug, Clone, PartialEq)]
#[view(databases = "sqlite")]
#[view(table = "media_type", key = "media_type_id")]
pub struct MediaTypeView {
    pub media_type_id: i32,
    pub name: Option<String>,
}

// Artists with their albums in a map keyed by title, and playlists of tracks

#[derive(View, Debug, Clone, PartialEq)]
#[view(databases = "sqlite")]
#[view(table = "artist", key = "artist_id")]
pub struct ArtistAlbums {
    pub artist_id: i32,
    pub name: Option<String>,
    #[view(child(fk = "artist_id", key = "title"))]
    pub albums: BTreeMap<String, AlbumTracks>,
}

#[derive(View, Debug, Clone, PartialEq)]
#[view(databases = "sqlite")]
#[view(table = "album", key = "album_id")]
pub struct AlbumTracks {
    pub album_id: i32,
    #[view(child(fk = "album_id", order_by = "track_id"))]
    pub tracks: Vec<TrackName>,
}

#[derive(View, Debug, Clone, PartialEq)]
#[view(databases = "sqlite")]
#[view(table = "track", key = "track_id")]
pub struct TrackName {
    pub track_id: i32,
    pub name: String,
    pub milliseconds: i32,
}

#[derive(View, Debug, Clone, PartialEq)]
#[view(databases = "sqlite")]
#[view(table = "playlist", key = "playlist_id")]
pub struct PlaylistView {
    pub playlist_id: i32,
    pub name: Option<String>,
    #[view(child(through = "playlist_track", fk = "playlist_id", target = "track_id", order_by = "track_id"))]
    pub tracks: Vec<TrackName>,
}

// The reporting hierarchy: a recursive structure

/// Loaded with one `WITH RECURSIVE` query.
#[derive(View, Debug, Clone, PartialEq)]
#[view(databases = "sqlite")]
#[view(table = "employee", key = "employee_id")]
pub struct EmployeeTree {
    pub employee_id: i32,
    pub title: Option<String>,
    #[view(child(fk = "reports_to", order_by = "employee_id", recursive = "cte"))]
    pub reports: Vec<EmployeeTree>,
}

/// Loaded level by level.
#[derive(View, Debug, Clone, PartialEq)]
#[view(databases = "sqlite")]
#[view(table = "employee", key = "employee_id")]
pub struct EmployeeLevels {
    pub employee_id: i32,
    pub title: Option<String>,
    #[view(child(fk = "reports_to", order_by = "employee_id", depth = 5))]
    pub reports: Vec<EmployeeLevels>,
}

// Employees, the customers they support and their invoices: a graph with cycles

#[derive(View, Debug)]
#[view(databases = "sqlite")]
#[view(table = "employee", key = "employee_id")]
pub struct Employee {
    pub employee_id: i32,
    pub first_name: String,
    pub last_name: String,
    pub title: Option<String>,
    #[view(to_one(fk = "reports_to"))]
    pub manager: Option<Ref<Employee>>,
    #[view(child(fk = "reports_to", order_by = "employee_id"))]
    pub reports: Vec<Ref<Employee>>,
    #[view(child(fk = "support_rep_id", order_by = "customer_id"))]
    pub customers: Vec<Ref<Customer>>,
}

#[derive(View, Debug)]
#[view(databases = "sqlite")]
#[view(table = "customer", key = "customer_id")]
pub struct Customer {
    pub customer_id: i32,
    pub first_name: String,
    pub last_name: String,
    pub country: Option<String>,
    #[view(to_one(fk = "support_rep_id"))]
    pub support_rep: Option<Ref<Employee>>,
    #[view(child(fk = "customer_id", order_by = "invoice_id"))]
    pub invoices: Vec<Ref<Invoice>>,
}

#[derive(View, Debug)]
#[view(databases = "sqlite")]
#[view(table = "invoice", key = "invoice_id")]
pub struct Invoice {
    pub invoice_id: i32,
    pub total: f64,
    #[view(to_one(fk = "customer_id"))]
    pub customer: Ref<Customer>,
}
