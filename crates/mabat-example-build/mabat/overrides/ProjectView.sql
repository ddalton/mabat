-- Overrides for ProjectView, tuned by the DBA team.

-- mabat: query tasks
SELECT t."id" AS "id", t."title" AS "title", t."done" AS "done", t."project_id" AS "$parent"
FROM "task" t
WHERE t."project_id" IN (:keys)
ORDER BY t."done", t."id";
