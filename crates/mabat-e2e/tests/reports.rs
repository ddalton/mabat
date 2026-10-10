//! Reports on Chinook on every database: a root query of the load's own SQL with named
//! parameters, whose rows have computed fields and still load their collections; ordered,
//! filtered, paged, counted and streamed like any load; and the same SQL as a checked override.

use chrono::NaiveDateTime;
use futures_util::TryStreamExt;
use mabat::filter::col;
use mabat::{Conn, Error, Mabat, Nested, View, ViewDecoder};
use mabat_e2e::Dataset;

/// The customers with the most invoices in a period, with those invoices.
#[derive(View, Debug, Clone, PartialEq)]
#[view(table = "customer", key = "customer_id")]
pub struct TopCustomer {
    pub customer_id: i32,
    pub last_name: String,
    #[view(computed)]
    pub invoices: i64,
    #[view(computed)]
    pub last_invoice: Option<NaiveDateTime>,
    #[view(child(fk = "customer_id", order_by = "invoice_date"))]
    pub period: Vec<InvoiceDate>,
}

#[derive(View, Debug, Clone, PartialEq)]
#[view(table = "invoice", key = "invoice_id")]
pub struct InvoiceDate {
    pub invoice_id: i32,
    pub invoice_date: NaiveDateTime,
}

/// The report: customers by the number of their invoices between `:from` and `:to`.
const REPORT: &str = r#"
SELECT c.customer_id AS "customer_id", c.last_name AS "last_name",
       count(*) AS "invoices", max(i.invoice_date) AS "last_invoice"
FROM customer c JOIN invoice i ON i.customer_id = c.customer_id
WHERE i.invoice_date >= :from AND i.invoice_date < :to
GROUP BY c.customer_id, c.last_name
"#;

fn date(text: &str) -> NaiveDateTime {
    NaiveDateTime::parse_from_str(&format!("{text} 00:00:00"), "%Y-%m-%d %H:%M:%S").unwrap()
}

/// The report for 2022, ordered by invoices, with each customer's invoices of the period.
fn report() -> mabat::Load<TopCustomer> {
    let (from, to) = (date("2022-01-01"), date("2023-01-01"));
    mabat::load::<TopCustomer>()
        .sql(REPORT)
        .bind("from", from)
        .bind("to", to)
        .nested("period", Nested::new().filter(col("invoice_date").ge(from) & col("invoice_date").lt(to)))
        .order_by_desc("invoices")
        .order_by("customer_id")
}

/// Each row agrees with its invoices: the count and the last date the SQL computed are those of
/// the invoices of the period that the collection loaded.
fn consistent(rows: &[TopCustomer]) {
    assert!(!rows.is_empty());
    for row in rows {
        assert_eq!(row.invoices, row.period.len() as i64, "{row:?}");
        assert_eq!(row.last_invoice, row.period.last().map(|i| i.invoice_date), "{row:?}");
    }
    // By invoices, most first, then by customer
    assert!(rows.windows(2).all(|w| (-w[0].invoices, w[0].customer_id) < (-w[1].invoices, w[1].customer_id)));
}

async fn reports<C: Conn>(conn: &mut C, keys_condition: &str)
where
    TopCustomer: ViewDecoder<C::Backend>,
{
    let all = report().all(&mut *conn).await.unwrap();
    consistent(&all);
    let customers: i64 = all.len() as i64;

    // Filtered by a computed field, paged, counted and streamed like any load
    let busy = report().filter(col("invoices").ge(2)).limit(3).offset(1).all(&mut *conn).await.unwrap();
    consistent(&busy);
    assert_eq!(busy, all.iter().filter(|r| r.invoices >= 2).skip(1).take(3).cloned().collect::<Vec<_>>());
    assert_eq!(report().count(&mut *conn).await.unwrap(), customers);
    let streamed: Vec<TopCustomer> = report().batch_size(7).stream(&mut *conn).try_collect().await.unwrap();
    assert_eq!(streamed, all);

    // By keys: the SQL filtered from outside, or taking the keys itself after a parameter
    let keys = [all[2].customer_id, all[0].customer_id];
    let by_keys = report().by_keys(keys).all(&mut *conn).await.unwrap();
    assert_eq!(by_keys, [all[0].clone(), all[2].clone()]);
    let taking_keys = REPORT.replace("GROUP BY", &format!("AND c.customer_id {keys_condition} GROUP BY"));
    let by_keys = report().sql(taking_keys).by_keys(keys).all(&mut *conn).await.unwrap();
    assert_eq!(by_keys, [all[0].clone(), all[2].clone()]);

    // Every parameter needs a value, and every value a parameter
    let missing = mabat::load::<TopCustomer>().sql(REPORT).bind("from", date("2022-01-01"));
    let error = missing.all(&mut *conn).await.unwrap_err();
    assert!(matches!(&error, Error::Params { .. }) && error.to_string().contains("`:to`"), "{error}");
    let extra = report().bind("since", 1);
    assert!(extra.all(&mut *conn).await.unwrap_err().to_string().contains("no `:since`"));
    // A computed field needs SQL to compute it
    let error = mabat::load::<TopCustomer>().all(&mut *conn).await.unwrap_err();
    assert!(error.to_string().contains("`invoices` is computed by SQL"), "{error}");
}

/// The report as an override of the root query, checked at startup, with the period bound.
async fn as_override<C: Conn>(conn: &mut C)
where
    TopCustomer: ViewDecoder<C::Backend>,
{
    let sql = format!("-- mabat: query $root\n{REPORT}");
    let mabat = Mabat::<C::Backend>::builder().register::<TopCustomer>().overrides_sql("TopCustomer", &sql);
    let mabat = mabat.build(&mut *conn).await.unwrap();
    assert!(mabat.report().diagnostics().is_empty(), "{}", mabat.report());
    let (from, to) = (date("2022-01-01"), date("2023-01-01"));
    let rows = mabat
        .load::<TopCustomer>()
        .bind("from", from)
        .bind("to", to)
        .nested("period", Nested::new().filter(col("invoice_date").ge(from) & col("invoice_date").lt(to)))
        .order_by_desc("invoices")
        .order_by("customer_id")
        .all(&mut *conn)
        .await
        .unwrap();
    assert_eq!(rows, report().all(&mut *conn).await.unwrap());

    // An override that leaves out a computed field is refused at startup
    let without = sql.replace(r#"count(*) AS "invoices", "#, "");
    let report = Mabat::<C::Backend>::builder()
        .register::<TopCustomer>()
        .overrides_sql("TopCustomer", &without)
        .check(&mut *conn)
        .await
        .unwrap();
    assert!(!report.is_ok() && report.to_string().contains("\"invoices\" is not selected"), "{report}");
}

#[tokio::test]
async fn sqlite() {
    let mut conn = Dataset::Chinook.connect_sqlite().await;
    reports(&mut conn, "IN (:keys)").await;
    as_override(&mut conn).await;
}

#[tokio::test]
async fn postgres() {
    let Some(mut conn) = Dataset::Chinook.connect().await else { return };
    reports(&mut conn, "= ANY(:keys)").await;
    as_override(&mut conn).await;
}

#[tokio::test]
async fn mysql() {
    let Some(mut conn) = Dataset::Chinook.connect_mysql().await else { return };
    reports(&mut conn, "IN (:keys)").await;
    as_override(&mut conn).await;
}
