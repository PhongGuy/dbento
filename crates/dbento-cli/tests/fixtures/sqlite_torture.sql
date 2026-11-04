-- crates/dbento-cli/tests/fixtures/sqlite_torture.sql
-- SQLite type torture (checklist A2c). Every table is read back with `select *`.
PRAGMA journal_mode = WAL;
CREATE TABLE affinity_mix (id INTEGER PRIMARY KEY, v);
INSERT INTO affinity_mix (v) VALUES (42), (2.5), ('text'), (x'00ff'), (NULL);
CREATE TABLE declared (a VARCHAR(10), b BOOLEAN, c DATETIME, d FLOATING POINT, e DECIMAL(10,2), f);
INSERT INTO declared VALUES ('longer than ten', 'true', '2024-01-02 03:04:05', '2.0', '1.10', '007');
INSERT INTO declared VALUES (1, 1, 1700000000, 1.5, 3, 7);
CREATE TABLE strict_t (i INTEGER, r REAL, t TEXT, b BLOB, a ANY) STRICT;
INSERT INTO strict_t VALUES (1, 2, 'x', x'01', '1'), (NULL, 0.5, '', x'', 1.0);
CREATE TABLE no_rowid (k TEXT PRIMARY KEY, v INT) WITHOUT ROWID;
INSERT INTO no_rowid VALUES ('b', 2), ('a', 1);
CREATE TABLE alias_pk (id INTEGER PRIMARY KEY, v TEXT);
INSERT INTO alias_pk VALUES (NULL, 'auto'), (10, 'ten');
CREATE TABLE int_pk (id INT PRIMARY KEY, v TEXT);
INSERT INTO int_pk VALUES (NULL, 'null key'), (10, 'ten');
CREATE TABLE edges (label TEXT, v);
INSERT INTO edges VALUES
  ('i64 max', 9223372036854775807), ('i64 min', -9223372036854775808),
  ('tenth', 0.1), ('huge', 1e300), ('max f64', 1.7976931348623157e308),
  ('min subnormal', 5e-324), ('negative zero', -0.0), ('infinity', 1e999),
  ('invalid utf-8', CAST(x'fffe41' AS TEXT)), ('nul inside', CAST(x'610062' AS TEXT)),
  ('emoji', 'bento 🍱'), ('quote and newline', 'say "hi"' || char(10) || 'bye'),
  ('empty text', ''), ('empty blob', x''), ('null', NULL);
CREATE TABLE generated (a INT, b INT GENERATED ALWAYS AS (a * 2) VIRTUAL, c TEXT AS (upper(n)) STORED, n TEXT);
INSERT INTO generated (a, n) VALUES (21, 'bento');
CREATE VIEW v_declared AS SELECT a, d, e FROM declared;
CREATE TABLE audited (v TEXT);
CREATE TABLE audit_log (id INTEGER PRIMARY KEY AUTOINCREMENT, msg TEXT);
CREATE TRIGGER audited_insert AFTER INSERT ON audited
BEGIN
  INSERT INTO audit_log (msg) VALUES ('inserted ' || NEW.v);
END;
INSERT INTO audited VALUES ('one'), ('two');
CREATE VIRTUAL TABLE docs USING fts5(title, body);
INSERT INTO docs VALUES ('Bento', 'rice and fish'), ('Jubako', 'stacked boxes');
