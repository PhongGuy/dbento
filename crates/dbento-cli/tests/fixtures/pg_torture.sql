-- Postgres type torture.
-- One table per family: rows of extremes and edge values, then a row of NULLs. Loaded with
-- `dbento run`, so each statement commits on its own, as a superuser on a fresh database.

CREATE TABLE num_t (id int PRIMARY KEY, s smallint, i int, b bigint, n numeric, n38 numeric(38,10), r real, d double precision, m money);
INSERT INTO num_t VALUES
  (1, -32768, -2147483648, -9223372036854775808, 12345678901234567890123456789012345678901234567890.5, 1234567890123456789012345678.0123456789, 'NaN', '-0', '-92233720368547758.08'),
  (2, 32767, 2147483647, 9223372036854775807, 'NaN', -0.0000000001, '1.4e-45', '4.9e-324', '92233720368547758.07'),
  (3, 0, 0, 0, 'Infinity', 0, '-Infinity', 'Infinity', 0),
  (4, 1, 1, 1, '-Infinity', 1, '3.4028235e38', '0.1', '1234.5'),
  (5, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL);

CREATE TABLE time_t (id int PRIMARY KEY, d date, ts timestamp, tz timestamptz, t time, ttz timetz, i interval);
INSERT INTO time_t VALUES
  (1, 'infinity', 'infinity', '-infinity', '24:00:00', '00:00:00+1459', '1 year 2 months 3 days 04:05:06.789012'),
  (2, '4713-01-01 BC', '0001-01-01 00:00:00 BC', '2024-03-10 12:00:00+00', '23:59:59.999999', '12:34:56-08', '-1 year -2 months +3 days -04:05:06'),
  (3, '2024-02-29', '2024-02-29 23:59:59.123456', '1999-12-31 23:59:59.5-05', '00:00:00', '00:00:00+00', '0'),
  (4, NULL, NULL, NULL, NULL, NULL, NULL);

CREATE TABLE text_t (id int PRIMARY KEY, t text, c char(5), v varchar(10), "quoted Name ü" text, "select" text);
INSERT INTO text_t VALUES
  (1, '', 'ab', 'bento 🍱', E'tab\tinside', 'reserved'),
  (2, '👨‍👩‍👧 family', 'é', 'مرحبا abc', 'שלום, world', E'new\nline'),
  (3, '日本語のテキスト', 'abcde', '', '"quoted"', 'back\slash'),
  (4, NULL, NULL, NULL, NULL, NULL);

CREATE TABLE bin_t (id int PRIMARY KEY, b bytea, bits bit(8), vb varbit);
INSERT INTO bin_t VALUES
  (1, '\x00ff10', B'10100101', B'1'),
  (2, '', B'00000000', B''),
  (3, NULL, NULL, NULL);

CREATE TYPE mood AS ENUM ('sad', 'happy');
ALTER TYPE mood ADD VALUE 'meh' BEFORE 'happy';
CREATE TYPE pair AS (a int, b text);
CREATE DOMAIN posint AS int CHECK (VALUE > 0);
CREATE DOMAIN int_list AS int[];
CREATE DOMAIN pair_d AS pair;
CREATE TABLE struct_t (id int PRIMARY KEY, j json, jb jsonb, x xml, a1 int[], a2 text[], a0 int[], ac pair[], p pair, m mood, dl int_list, dp pair_d, pi posint);
INSERT INTO struct_t VALUES
  (1, '{"b": 1, "a": 2, "a": 3}', '{"b": 1, "a": 2, "a": 3}', '<r a="1">x &amp; y</r>', '{1,NULL,3}', '{{"a","b"},{"c",NULL}}', '[0:2]={1,2,3}', ARRAY[ROW(1, 'x')::pair, ROW(NULL, 'y "q"')::pair], ROW(1, 'a,b'), 'meh', '{4,5}', ROW(2, NULL), 7),
  (2, '[[[[[[[[[[1]]]]]]]]]]', '[[[[[[[[[[1]]]]]]]]]]', 'plain text', '{}', '{"",NULL}', '[-1:-1]={0}', '{}', ROW(NULL, NULL), 'happy', '{}', ROW(NULL, ''), 1),
  (3, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL);

CREATE TABLE range_t (id int PRIMARY KEY, i4 int4range, i8 int8range, nr numrange, tsr tsrange, tstzr tstzrange, dr daterange, mr int4multirange, dmr datemultirange);
INSERT INTO range_t VALUES
  (1, '[1,10)', '(,5]', 'empty', '[2024-01-01,)', '[2024-01-01 00:00+00,2024-01-02 00:00+00)', '[2024-01-01,2024-02-01]', '{[1,3),[5,7)}', '{}'),
  (2, 'empty', '(,)', '[1.5,2.25]', '(,)', 'empty', '(2024-01-01,2024-01-02)', '{}', '{[2024-01-01,2024-01-05], [2024-01-03,2024-01-09)}'),
  (3, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL);

CREATE TABLE net_t (id int PRIMARY KEY, ip inet, c cidr, m macaddr, m8 macaddr8, u uuid);
INSERT INTO net_t VALUES
  (1, '192.168.0.1/24', '10.0.0.0/8', '08:00:2b:01:02:03', '08:00:2b:01:02:03:04:05', 'A0EEBC99-9C0B-4EF8-BB6D-6BB9BD380A11'),
  (2, '::ffff:1.2.3.4', '2001:db8::/32', 'ff:ff:ff:ff:ff:ff', '0800.2b01.0203', '00000000-0000-0000-0000-000000000000'),
  (3, NULL, NULL, NULL, NULL, NULL);

CREATE EXTENSION citext;
CREATE EXTENSION hstore;
CREATE TABLE other_t (id int PRIMARY KEY, pt point, ln line, ls lseg, bx box, pa path, pg polygon, ci circle, tv tsvector, tq tsquery, lsn pg_lsn, o oid, rc regclass, rt regtype, snap pg_snapshot, ts txid_snapshot, ct citext, hs hstore);
INSERT INTO other_t VALUES
  (1, '(1.5,-2)', '{1,-1,0}', '[(0,0),(1,1)]', '(1,1),(0,0)', '[(0,0),(1,1),(2,0)]', '((0,0),(1,1),(1,0))', '<(0,0),5>', 'a fat cat:2A sat', 'fat & (rat | !cat)', '16/B374D848', 4294967295, 'pg_class', 'int4', '10:20:10,14,15', '10:20:10,14,15', 'MiXeD', 'a=>1, "b c"=>NULL'),
  (2, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL);

CREATE TABLE shape_t (id int GENERATED ALWAYS AS IDENTITY PRIMARY KEY, a int, g int GENERATED ALWAYS AS (a * 2) STORED);
INSERT INTO shape_t (a) VALUES (21), (NULL);
