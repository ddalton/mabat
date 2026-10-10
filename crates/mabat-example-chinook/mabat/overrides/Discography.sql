-- A DBA's override: an artist's albums newest first, by album id, instead of by title.
-- The query keeps the aliases the view decodes; `mabat check` and the build script check them.

-- mabat: query albums
SELECT a.album_id AS "album_id", a.artist_id AS "$parent", a.title AS "title"
FROM album AS a
WHERE a.artist_id IN (:keys)
ORDER BY a.album_id DESC
