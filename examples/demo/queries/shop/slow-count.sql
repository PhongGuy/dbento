-- A long recursive count: press Cancel while it runs.
WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 500000000)
SELECT count(*) FROM n;
