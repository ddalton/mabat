-- A report a DBA maintains: the customers who spent the most in a period. It is checked at startup and by the
-- build script like any override, computed fields included; each request binds :from and :to.

-- mabat: query $root
SELECT c.customer_id AS "customer_id", c.first_name AS "first_name", c.last_name AS "last_name",
       c.country AS "country", count(*) AS "invoices", sum(i.total) AS "spent"
FROM customer AS c
JOIN invoice AS i ON i.customer_id = c.customer_id
WHERE i.invoice_date >= :from AND i.invoice_date < :to
GROUP BY c.customer_id, c.first_name, c.last_name, c.country
