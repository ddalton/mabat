//! Views of Pagila, a DVD rental store.

use std::sync::Arc;

use chrono::{DateTime, NaiveDate, Utc};
use mabat::{Ref, View};
use rust_decimal::Decimal;

// Films: an enum from a PostgreSQL enum, a domain, an array, money, many-to-many links

#[derive(View, Debug, Clone, PartialEq)]
#[view(databases = "postgres")]
#[view(table = "film", key = "film_id")]
pub struct FilmView {
    pub film_id: i32,
    pub title: String,
    pub description: Option<String>,
    /// A `year` domain over `integer`.
    pub release_year: Option<i32>,
    pub length: Option<i16>,
    pub rental_rate: Decimal,
    pub special_features: Option<Vec<String>>,
    /// The `mpaa_rating` PostgreSQL enum.
    #[view(embed)]
    pub rating: Rating,
    #[view(to_one(fk = "language_id"))]
    pub language: Arc<LanguageView>,
    #[view(to_one(fk = "original_language_id"))]
    pub original_language: Option<Arc<LanguageView>>,
    #[view(child(through = "film_actor", fk = "film_id", target = "actor_id", order_by = "last_name, first_name"))]
    pub actors: Vec<ActorName>,
    #[view(child(through = "film_category", fk = "film_id", target = "category_id", order_by = "name"))]
    pub categories: Vec<CategoryName>,
}

#[derive(View, Debug, Clone, Copy, PartialEq, Eq)]
#[view(databases = "postgres")]
#[view(tag = "rating")]
pub enum Rating {
    #[view(tag_value = "G")]
    G,
    #[view(tag_value = "PG")]
    Pg,
    #[view(tag_value = "PG-13")]
    Pg13,
    #[view(tag_value = "R")]
    R,
    #[view(tag_value = "NC-17")]
    Nc17,
}

impl Rating {
    pub fn as_str(self) -> &'static str {
        match self {
            Rating::G => "G",
            Rating::Pg => "PG",
            Rating::Pg13 => "PG-13",
            Rating::R => "R",
            Rating::Nc17 => "NC-17",
        }
    }
}

#[derive(View, Debug, Clone, PartialEq)]
#[view(databases = "postgres")]
#[view(table = "language", key = "language_id")]
pub struct LanguageView {
    pub language_id: i32,
    /// `character(20)`
    pub name: String,
}

#[derive(View, Debug, Clone, PartialEq)]
#[view(databases = "postgres")]
#[view(table = "actor", key = "actor_id")]
pub struct ActorName {
    pub actor_id: i32,
    pub first_name: String,
    pub last_name: String,
}

#[derive(View, Debug, Clone, PartialEq)]
#[view(databases = "postgres")]
#[view(table = "category", key = "category_id")]
pub struct CategoryName {
    pub category_id: i32,
    pub name: String,
}

// Customers: a chain of references, rentals and payments from a partitioned table

#[derive(View, Debug, Clone, PartialEq)]
#[view(databases = "postgres")]
#[view(table = "customer", key = "customer_id")]
pub struct CustomerView {
    pub customer_id: i32,
    pub first_name: String,
    pub last_name: String,
    pub email: Option<String>,
    pub activebool: bool,
    pub create_date: NaiveDate,
    #[view(to_one(fk = "address_id"))]
    pub address: AddressView,
    #[view(child(fk = "customer_id", order_by = "rental_date, rental_id"))]
    pub rentals: Vec<RentalView>,
}

#[derive(View, Debug, Clone, PartialEq)]
#[view(databases = "postgres")]
#[view(table = "address", key = "address_id")]
pub struct AddressView {
    pub address: String,
    pub district: String,
    pub postal_code: Option<String>,
    pub phone: String,
    #[view(to_one(fk = "city_id"))]
    pub city: Arc<CityView>,
}

#[derive(View, Debug, Clone, PartialEq)]
#[view(databases = "postgres")]
#[view(table = "city", key = "city_id")]
pub struct CityView {
    pub city: String,
    #[view(to_one(fk = "country_id"))]
    pub country: Arc<CountryView>,
}

#[derive(View, Debug, Clone, PartialEq)]
#[view(databases = "postgres")]
#[view(table = "country", key = "country_id")]
pub struct CountryView {
    pub country: String,
}

#[derive(View, Debug, Clone, PartialEq)]
#[view(databases = "postgres")]
#[view(table = "rental", key = "rental_id")]
pub struct RentalView {
    pub rental_id: i32,
    pub rental_date: DateTime<Utc>,
    pub return_date: Option<DateTime<Utc>>,
    #[view(to_one(fk = "inventory_id"))]
    pub inventory: InventoryView,
    /// From the partitioned `payment` table.
    #[view(child(fk = "rental_id", order_by = "payment_id"))]
    pub payments: Vec<PaymentView>,
}

#[derive(View, Debug, Clone, PartialEq)]
#[view(databases = "postgres")]
#[view(table = "inventory", key = "inventory_id")]
pub struct InventoryView {
    pub store_id: i32,
    #[view(to_one(fk = "film_id"))]
    pub film: Arc<FilmTitle>,
}

#[derive(View, Debug, Clone, PartialEq)]
#[view(databases = "postgres")]
#[view(table = "film", key = "film_id")]
pub struct FilmTitle {
    pub film_id: i32,
    pub title: String,
}

#[derive(View, Debug, Clone, PartialEq)]
#[view(databases = "postgres")]
#[view(table = "payment", key = "payment_id")]
pub struct PaymentView {
    pub payment_id: i32,
    pub amount: Decimal,
    pub payment_date: DateTime<Utc>,
}

// An enum the schema does not have, derived by an override from the return date

#[derive(View, Debug, Clone, PartialEq)]
#[view(databases = "postgres")]
#[view(table = "rental", key = "rental_id")]
pub struct RentalStatusView {
    pub rental_id: i32,
    #[view(embed)]
    pub status: RentalStatus,
}

#[derive(View, Debug, Clone, PartialEq)]
#[view(databases = "postgres")]
#[view(tag = "status")]
pub enum RentalStatus {
    #[view(tag_value = "returned")]
    Returned {
        #[view(column = "return_date")]
        at: DateTime<Utc>,
    },
    #[view(tag_value = "outstanding")]
    Outstanding,
}

pub const RENTAL_STATUS_OVERRIDE: &str = r#"
-- mabat: query $root
SELECT r.rental_id AS "rental_id",
       CASE WHEN r.return_date IS NULL THEN 'outstanding' ELSE 'returned' END AS "status.$tag",
       r.return_date AS "status.Returned.at"
FROM rental r
"#;

// Stock computed by a stored function

#[derive(View, Debug, Clone, PartialEq)]
#[view(databases = "postgres")]
#[view(table = "film", key = "film_id")]
pub struct FilmStock {
    pub film_id: i32,
    pub title: String,
    /// Copies in store 1 that are not rented out, from the `film_in_stock` function.
    pub in_store_1: i64,
}

pub const FILM_STOCK_OVERRIDE: &str = r#"
-- mabat: query $root
SELECT f.film_id AS "film_id", f.title AS "title",
       (SELECT count(*) FROM film_in_stock(f.film_id, 1)) AS "in_store_1"
FROM film f
"#;

// Stores and their staff reference each other: a graph

#[derive(View, Debug)]
#[view(databases = "postgres")]
#[view(table = "store", key = "store_id")]
pub struct Store {
    pub store_id: i32,
    #[view(to_one(fk = "manager_staff_id"))]
    pub manager: Ref<Staff>,
    #[view(child(fk = "store_id", order_by = "staff_id"))]
    pub staff: Vec<Ref<Staff>>,
    #[view(to_one(fk = "address_id"))]
    pub address: AddressView,
}

#[derive(View, Debug)]
#[view(databases = "postgres")]
#[view(table = "staff", key = "staff_id")]
pub struct Staff {
    pub staff_id: i32,
    pub first_name: String,
    pub last_name: String,
    #[view(to_one(fk = "store_id"))]
    pub store: Ref<Store>,
}

// Films and actors: a large connected graph through the link table

#[derive(View, Debug)]
#[view(databases = "postgres")]
#[view(table = "film", key = "film_id")]
pub struct FilmNode {
    pub film_id: i32,
    pub title: String,
    #[view(child(through = "film_actor", fk = "film_id", target = "actor_id", order_by = "actor_id"))]
    pub actors: Vec<Ref<ActorNode>>,
}

#[derive(View, Debug)]
#[view(databases = "postgres")]
#[view(table = "actor", key = "actor_id")]
pub struct ActorNode {
    pub actor_id: i32,
    pub first_name: String,
    pub last_name: String,
    #[view(child(through = "film_actor", fk = "actor_id", target = "film_id", order_by = "film_id"))]
    pub films: Vec<Ref<FilmNode>>,
}
