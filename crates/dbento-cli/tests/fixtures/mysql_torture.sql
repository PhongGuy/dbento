-- MySQL and MariaDB type torture. One table per family: rows of extremes and edge values,
-- then a row of NULLs. Loaded with `dbento run` as root on a fresh database; sql_mode is
-- cleared first so zero dates load, and the time zone is fixed so TIMESTAMP reads back stable.
SET SESSION sql_mode = '';
SET SESSION time_zone = '+00:00';

CREATE TABLE num_t (id INT PRIMARY KEY, ti TINYINT, si SMALLINT UNSIGNED, mi MEDIUMINT, i INT, bi BIGINT, ub BIGINT UNSIGNED, d DECIMAL(65,30), f FLOAT, db DOUBLE, y YEAR);
INSERT INTO num_t VALUES
  (1, -128, 0, -8388608, -2147483648, -9223372036854775808, 0, -12345678901234567890123456789012345.123456789012345678901234567890, -3.40282e38, -1.7976931348623157e308, 1901),
  (2, 127, 65535, 8388607, 2147483647, 9223372036854775807, 18446744073709551615, 0.000000000000000000000000000001, 1.17549e-38, 2.2250738585072014e-308, 2155),
  (3, 0, 1, 0, 0, 0, 1, 0, 0, -0, 0),
  (4, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL);

CREATE TABLE time_t (id INT PRIMARY KEY, d DATE, dt DATETIME(6), ts TIMESTAMP(3) NULL, t TIME(1));
INSERT INTO time_t VALUES
  (1, '0000-00-00', '0000-00-00 00:00:00', '1970-01-01 00:00:01.000', '-838:59:59.0'),
  (2, '2020-00-00', '9999-12-31 23:59:59.999999', '2038-01-19 03:14:07.999', '838:59:59.0'),
  (3, '2024-02-29', '1000-01-01 00:00:00.000001', '2024-02-29 12:34:56.789', '00:00:00.5'),
  (4, NULL, NULL, NULL, NULL);

CREATE TABLE text_t (id INT PRIMARY KEY, c CHAR(5), v VARCHAR(40), t TEXT, `quoted Name ü` VARCHAR(10), `select` VARCHAR(10), e ENUM('small','large'), s SET('a','b','c'));
INSERT INTO text_t VALUES
  (1, 'ab', 'bento 🍱', '', 'tab\tinside', 'back\\slash', 'small', 'a,c'),
  (2, 'é', '日本語 text', 'new\nline', '"quoted"', 'it''s', 'large', ''),
  (3, NULL, NULL, NULL, NULL, NULL, NULL, NULL);

CREATE TABLE bin_t (id INT PRIMARY KEY, b BINARY(3), vb VARBINARY(8), bl BLOB, bits BIT(10), g GEOMETRY NULL);
INSERT INTO bin_t VALUES
  (1, X'00FF10', X'', X'C328', b'1010101010', ST_GeomFromText('POINT(1 2)')),
  (2, X'616263', X'FFFE', X'', b'0', NULL),
  (3, NULL, NULL, NULL, NULL, NULL);

CREATE TABLE json_t (id INT PRIMARY KEY, j JSON);
INSERT INTO json_t VALUES
  (1, '{"b": 1, "a": [1, "two", null]}'),
  (2, '"text"'),
  (3, NULL);
