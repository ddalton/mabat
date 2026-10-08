//! End-to-end tests against Pagila: every load is compared with an independent answer
//! computed in SQL.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Instant;

use mabat::filter::col;
use mabat::manifest::ScaffoldFormat;
use mabat::{Error, Mabat};
use mabat_e2e::Dataset;
use mabat_e2e::pagila::*;
use rust_decimal::Decimal;
use sqlx::{AssertSqlSafe, PgConnection, Row};

async fn pagila() -> Option<PgConnection> {
    Dataset::Pagila.connect().await
}

async fn count(conn: &mut PgConnection, sql: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(AssertSqlSafe(sql.to_string())).fetch_one(conn).await.unwrap()
}

#[tokio::test]
async fn films_match_independent_sql() {
    let Some(mut conn) = pagila().await else { return };

    let start = Instant::now();
    let films = mabat::load::<FilmView>().order_by("film_id").all(&mut conn).await.unwrap();
    eprintln!("loaded {} films with actors and categories in {:?}", films.len(), start.elapsed());

    let expected = sqlx::query(
        "SELECT f.film_id, f.title, f.release_year::int AS release_year, f.length, f.rental_rate,
                f.special_features, f.rating::text AS rating, trim(l.name) AS language,
                coalesce((SELECT string_agg(a.first_name || ' ' || a.last_name, '|' ORDER BY a.last_name, a.first_name)
                          FROM film_actor fa JOIN actor a ON a.actor_id = fa.actor_id WHERE fa.film_id = f.film_id), '') AS actors,
                coalesce((SELECT string_agg(c.name, '|' ORDER BY c.name)
                          FROM film_category fc JOIN category c ON c.category_id = fc.category_id
                          WHERE fc.film_id = f.film_id), '') AS categories
         FROM film f JOIN language l ON l.language_id = f.language_id ORDER BY f.film_id",
    )
    .fetch_all(&mut conn)
    .await
    .unwrap();
    assert_eq!(films.len(), 1000);
    assert_eq!(films.len(), expected.len());

    for (film, row) in films.iter().zip(&expected) {
        let id = film.film_id;
        assert_eq!(film.title, row.get::<&str, _>("title"));
        assert_eq!(film.release_year, row.get::<Option<i32>, _>("release_year"), "film {id}");
        assert_eq!(film.length, row.get::<Option<i16>, _>("length"), "film {id}");
        assert_eq!(film.rental_rate, row.get::<Decimal, _>("rental_rate"), "film {id}");
        assert_eq!(film.special_features, row.get::<Option<Vec<String>>, _>("special_features"), "film {id}");
        assert_eq!(film.rating.as_str(), row.get::<&str, _>("rating"), "film {id}");
        assert_eq!(film.language.name.trim(), row.get::<&str, _>("language"));
        let actors: Vec<String> = film.actors.iter().map(|a| format!("{} {}", a.first_name, a.last_name)).collect();
        assert_eq!(actors.join("|"), row.get::<&str, _>("actors"), "film {id}");
        let categories: Vec<&str> = film.categories.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(categories.join("|"), row.get::<&str, _>("categories"), "film {id}");
    }

    // Every film is in English: one shared allocation
    assert!(films.windows(2).all(|w| Arc::ptr_eq(&w[0].language, &w[1].language)));
    assert!(films.iter().all(|f| f.original_language.is_none()));
}

#[tokio::test]
async fn filters_on_enums_domains_and_money() {
    let Some(mut conn) = pagila().await else { return };

    // The rating tag is decoded from the PostgreSQL enum
    let films = mabat::load::<FilmView>().all(&mut conn).await.unwrap();
    let mut by_rating: BTreeMap<&str, i64> = BTreeMap::new();
    for film in &films {
        *by_rating.entry(film.rating.as_str()).or_default() += 1;
    }
    let expected: BTreeMap<String, i64> =
        sqlx::query("SELECT rating::text AS rating, count(*) AS n FROM film GROUP BY 1")
            .fetch_all(&mut conn)
            .await
            .unwrap()
            .iter()
            .map(|r| (r.get("rating"), r.get("n")))
            .collect();
    assert_eq!(by_rating.iter().map(|(k, v)| (k.to_string(), *v)).collect::<BTreeMap<_, _>>(), expected);

    let cases = [
        (col("release_year").eq(2006_i32), "release_year = 2006"),
        (col("length").gt(150_i16), "length > 150"),
        (col("rental_rate").ge(2.99_f64), "rental_rate >= 2.99"),
        (
            col("title").ilike("%love%") | col("description").ilike("%drama%"),
            "title ILIKE '%love%' OR description ILIKE '%drama%'",
        ),
        (
            col("original_language_id").is_null() & col("language_id").is_in([1_i32, 2]),
            "original_language_id IS NULL AND language_id IN (1, 2)",
        ),
    ];
    for (condition, sql) in cases {
        let n = mabat::load::<FilmView>().filter(condition).count(&mut conn).await.unwrap();
        assert_eq!(n, count(&mut conn, &format!("SELECT count(*) FROM film WHERE {sql}")).await, "{sql}");
    }

    let longest: Vec<String> = mabat::load::<FilmTitle>()
        .order_by_desc("length")
        .order_by("title")
        .limit(3)
        .all(&mut conn)
        .await
        .unwrap()
        .into_iter()
        .map(|f| f.title)
        .collect();
    let expected: Vec<String> = sqlx::query_scalar("SELECT title FROM film ORDER BY length DESC, title LIMIT 3")
        .fetch_all(&mut conn)
        .await
        .unwrap();
    assert_eq!(longest, expected);
}

#[tokio::test]
async fn customers_with_rentals_and_partitioned_payments() {
    let Some(mut conn) = pagila().await else { return };

    let start = Instant::now();
    let customers = mabat::load::<CustomerView>().order_by("customer_id").all(&mut conn).await.unwrap();
    let rentals: usize = customers.iter().map(|c| c.rentals.len()).sum();
    let payments: usize = customers.iter().flat_map(|c| &c.rentals).map(|r| r.payments.len()).sum();
    eprintln!(
        "loaded {} customers, {rentals} rentals and {payments} payments in {:?}",
        customers.len(),
        start.elapsed()
    );
    assert_eq!(rentals as i64, count(&mut conn, "SELECT count(*) FROM rental").await);
    assert_eq!(payments as i64, count(&mut conn, "SELECT count(*) FROM payment").await);

    let expected = sqlx::query(
        "SELECT c.customer_id, co.country, ci.city,
                (SELECT count(*) FROM rental r WHERE r.customer_id = c.customer_id) AS rentals,
                (SELECT coalesce(sum(p.amount), 0) FROM payment p JOIN rental r ON r.rental_id = p.rental_id
                 WHERE r.customer_id = c.customer_id) AS paid,
                (SELECT string_agg(f.title, '|' ORDER BY r.rental_date, r.rental_id)
                 FROM rental r JOIN inventory i ON i.inventory_id = r.inventory_id JOIN film f ON f.film_id = i.film_id
                 WHERE r.customer_id = c.customer_id) AS titles
         FROM customer c JOIN address a ON a.address_id = c.address_id JOIN city ci ON ci.city_id = a.city_id
         JOIN country co ON co.country_id = ci.country_id ORDER BY c.customer_id",
    )
    .fetch_all(&mut conn)
    .await
    .unwrap();
    assert_eq!(customers.len(), expected.len());
    for (customer, row) in customers.iter().zip(&expected) {
        let id = customer.customer_id;
        assert_eq!(customer.address.city.country.country, row.get::<&str, _>("country"));
        assert_eq!(customer.address.city.city, row.get::<&str, _>("city"));
        assert_eq!(customer.rentals.len() as i64, row.get::<i64, _>("rentals"), "customer {id}");
        let paid: Decimal = customer.rentals.iter().flat_map(|r| &r.payments).map(|p| p.amount).sum();
        assert_eq!(paid, row.get::<Decimal, _>("paid"), "customer {id}");
        let titles: Vec<&str> = customer.rentals.iter().map(|r| r.inventory.film.title.as_str()).collect();
        assert_eq!(titles.join("|"), row.get::<Option<&str>, _>("titles").unwrap_or(""), "customer {id}");
    }

    // Cities and countries are shared across customers, films across rentals
    let countries: BTreeSet<*const CountryView> =
        customers.iter().map(|c| Arc::as_ptr(&c.address.city.country)).collect();
    let expected = count(
        &mut conn,
        "SELECT count(DISTINCT ci.country_id) FROM customer c JOIN address a ON a.address_id = c.address_id
         JOIN city ci ON ci.city_id = a.city_id",
    )
    .await;
    assert_eq!(countries.len() as i64, expected);
    let mut films: HashMap<i32, &Arc<FilmTitle>> = HashMap::new();
    for rental in customers.iter().flat_map(|c| &c.rentals) {
        assert!(Arc::ptr_eq(
            films.entry(rental.inventory.film.film_id).or_insert(&rental.inventory.film),
            &rental.inventory.film
        ));
    }
}

#[tokio::test]
async fn overrides_derive_an_enum_and_call_a_stored_function() {
    let Some(mut conn) = pagila().await else { return };

    // An enum from the return date
    let err = mabat::load::<RentalStatusView>().all(&mut conn).await.unwrap_err();
    assert!(matches!(err, Error::Query { .. }), "the schema has no status column: {err}");
    let mabat = Mabat::builder()
        .register::<RentalStatusView>()
        .overrides_sql("RentalStatusView", RENTAL_STATUS_OVERRIDE)
        .register::<FilmStock>()
        .overrides_sql("FilmStock", FILM_STOCK_OVERRIDE)
        .build(&mut conn)
        .await
        .unwrap();
    assert!(mabat.report().is_ok(), "{}", mabat.report());
    assert_eq!(mabat.report().warnings().count(), 2, "{}", mabat.report());

    let rentals = mabat.load::<RentalStatusView>().all(&mut conn).await.unwrap();
    let outstanding = rentals.iter().filter(|r| r.status == RentalStatus::Outstanding).count();
    assert_eq!(outstanding as i64, count(&mut conn, "SELECT count(*) FROM rental WHERE return_date IS NULL").await);
    let returned: BTreeMap<i32, _> = rentals
        .iter()
        .filter_map(|r| match r.status {
            RentalStatus::Returned { at } => Some((r.rental_id, at)),
            RentalStatus::Outstanding => None,
        })
        .collect();
    let expected: BTreeMap<i32, chrono::DateTime<chrono::Utc>> =
        sqlx::query("SELECT rental_id, return_date FROM rental WHERE return_date IS NOT NULL")
            .fetch_all(&mut conn)
            .await
            .unwrap()
            .iter()
            .map(|r| (r.get("rental_id"), r.get("return_date")))
            .collect();
    assert_eq!(returned, expected);

    // A stored function in an override: copies in store 1 that are not rented out
    let stock = mabat.load::<FilmStock>().order_by("film_id").all(&mut conn).await.unwrap();
    let expected: Vec<(i32, i64)> = sqlx::query(
        "SELECT f.film_id, count(i.inventory_id) FILTER (WHERE NOT EXISTS (
                    SELECT 1 FROM rental r WHERE r.inventory_id = i.inventory_id AND r.return_date IS NULL)) AS in_stock
         FROM film f LEFT JOIN inventory i ON i.film_id = f.film_id AND i.store_id = 1
         GROUP BY f.film_id ORDER BY f.film_id",
    )
    .fetch_all(&mut conn)
    .await
    .unwrap()
    .iter()
    .map(|r| (r.get("film_id"), r.get("in_stock")))
    .collect();
    let loaded: Vec<(i32, i64)> = stock.iter().map(|f| (f.film_id, f.in_store_1)).collect();
    assert_eq!(loaded, expected);
}

#[tokio::test]
async fn stores_and_staff_reference_each_other() {
    let Some(mut conn) = pagila().await else { return };

    let graph = mabat::load::<Store>().order_by("store_id").graph(&mut conn).await.unwrap();
    assert_eq!(graph.count::<Store>() as i64, count(&mut conn, "SELECT count(*) FROM store").await);
    assert_eq!(graph.count::<Staff>() as i64, count(&mut conn, "SELECT count(*) FROM staff").await);
    for store in graph.roots() {
        // The manager works at the store, and the store's staff point back to it
        let manager = store.manager(&graph);
        assert_eq!(graph.get(manager.store).store_id, store.store_id);
        for member in store.staff(&graph) {
            assert_eq!(member.store(&graph).store_id, store.store_id);
        }
        assert!(store.staff.contains(&store.manager));
    }
    let expected: Vec<(i32, String)> = sqlx::query(
        "SELECT s.store_id, st.first_name || ' ' || st.last_name AS manager FROM store s
         JOIN staff st ON st.staff_id = s.manager_staff_id ORDER BY s.store_id",
    )
    .fetch_all(&mut conn)
    .await
    .unwrap()
    .iter()
    .map(|r| (r.get("store_id"), r.get("manager")))
    .collect();
    let managers: Vec<(i32, String)> = graph
        .roots()
        .map(|s| (s.store_id, format!("{} {}", s.manager(&graph).first_name, s.manager(&graph).last_name)))
        .collect();
    assert_eq!(managers, expected);
}

#[tokio::test]
async fn films_and_actors_form_one_large_graph() {
    let Some(mut conn) = pagila().await else { return };

    let start = Instant::now();
    let graph = mabat::load::<ActorNode>().by_key(1_i32).graph(&mut conn).await.unwrap();
    eprintln!(
        "loaded a graph of {} actors and {} films in {:?}",
        graph.count::<ActorNode>(),
        graph.count::<FilmNode>(),
        start.elapsed()
    );

    // The connected component of actor 1, computed by a recursive query over the link table
    let component = sqlx::query(
        "WITH RECURSIVE reached(kind, id) AS (
             SELECT 'actor', 1
             UNION
             SELECT CASE WHEN r.kind = 'actor' THEN 'film' ELSE 'actor' END,
                    CASE WHEN r.kind = 'actor' THEN fa.film_id ELSE fa.actor_id END
             FROM reached r JOIN film_actor fa
               ON (r.kind = 'actor' AND fa.actor_id = r.id) OR (r.kind = 'film' AND fa.film_id = r.id))
         SELECT count(*) FILTER (WHERE kind = 'actor') AS actors, count(*) FILTER (WHERE kind = 'film') AS films FROM reached",
    )
    .fetch_one(&mut conn)
    .await
    .unwrap();
    assert_eq!(graph.count::<ActorNode>() as i64, component.get::<i64, _>("actors"));
    assert_eq!(graph.count::<FilmNode>() as i64, component.get::<i64, _>("films"));

    // Both directions of every link agree with the link table
    let links: BTreeSet<(i32, i32)> = sqlx::query("SELECT actor_id, film_id FROM film_actor")
        .fetch_all(&mut conn)
        .await
        .unwrap()
        .iter()
        .map(|r| (r.get("actor_id"), r.get("film_id")))
        .collect();
    let mut from_actors = BTreeSet::new();
    for (actor_ref, actor) in graph.all::<ActorNode>() {
        for film in actor.films(&graph) {
            from_actors.insert((actor.actor_id, film.film_id));
            assert!(film.actors.contains(&actor_ref));
        }
    }
    let reached_links: BTreeSet<(i32, i32)> =
        links.iter().filter(|(a, _)| graph.all::<ActorNode>().any(|(_, x)| x.actor_id == *a)).copied().collect();
    assert_eq!(from_actors, reached_links);
}

/// Checks pass for every Pagila view with its overrides, and for the scaffolded override
/// file of each view.
#[tokio::test]
async fn every_view_and_scaffold_passes_the_checks() {
    let Some(mut conn) = pagila().await else { return };

    fn views() -> mabat::Builder<sqlx::Postgres> {
        Mabat::builder()
            .register::<FilmView>()
            .register::<CustomerView>()
            .register::<Store>()
            .register::<ActorNode>()
            .register::<RentalStatusView>()
            .register::<FilmStock>()
    }
    let report = views()
        .overrides_sql("RentalStatusView", RENTAL_STATUS_OVERRIDE)
        .overrides_sql("FilmStock", FILM_STOCK_OVERRIDE)
        .check(&mut conn)
        .await
        .unwrap();
    assert!(report.is_ok(), "{report}");

    let manifest = views().manifest().unwrap();
    for view in ["FilmView", "CustomerView", "Store", "ActorNode"] {
        let scaffold = manifest.scaffold(view, ScaffoldFormat::Sql).unwrap();
        let report = views()
            .overrides_sql("RentalStatusView", RENTAL_STATUS_OVERRIDE)
            .overrides_sql("FilmStock", FILM_STOCK_OVERRIDE)
            .overrides_sql(view, scaffold)
            .check(&mut conn)
            .await
            .unwrap();
        assert!(report.is_ok() && report.warnings().count() == 2, "{view}: {report}");
    }
}
