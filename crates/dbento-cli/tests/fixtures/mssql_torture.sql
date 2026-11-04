-- The SQL Server type-torture fixture: one table per family, its extremes, then a NULL row.
CREATE TABLE num_t (id int NOT NULL PRIMARY KEY, b bit, ti tinyint, si smallint, i int, bi bigint, r real, f float, d decimal(38,10), m money, sm smallmoney);
INSERT INTO num_t VALUES
(1, 0, 0, -32768, -2147483648, -9223372036854775808, -3.4028235E+38, -1.7976931348623157E+308, -9999999999999999999999999999.9999999999, -922337203685477.5808, -214748.3648),
(2, 1, 255, 32767, 2147483647, 9223372036854775807, 1.17549435E-38, 2.2250738585072014E-308, 0.0000000001, 922337203685477.5807, 214748.3647),
(3, 1, 1, 0, 0, 0, 1.5, 1E+20, 0.05, 0.0001, 0),
(4, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL);
CREATE TABLE time_t (id int NOT NULL PRIMARY KEY, d date, dt datetime, sdt smalldatetime, dt2 datetime2(7), t time(7), dto datetimeoffset(7), t0 time(0));
INSERT INTO time_t VALUES
(1, '0001-01-01', '1753-01-01T00:00:00', '1900-01-01T00:00:00', '0001-01-01T00:00:00', '00:00:00', '0001-01-01T00:00:00-14:00', '00:00:00'),
(2, '9999-12-31', '9999-12-31T23:59:59.997', '2079-06-06T23:59:00', '9999-12-31T23:59:59.9999999', '23:59:59.9999999', '9999-12-31T23:59:59.9999999+14:00', '23:59:59'),
(3, '2024-02-29', '2024-02-29T12:34:56.789', '2024-02-29T12:34:30', '2024-02-29T12:34:56.1234567', '12:34:56.5', '2024-02-29T12:34:56.1234567+05:30', '12:34:56'),
(4, NULL, NULL, NULL, NULL, NULL, NULL, NULL);
CREATE TABLE text_t (id int NOT NULL PRIMARY KEY, c char(3), v varchar(20), nv nvarchar(20), nm nvarchar(max), tx text, ntx ntext, x xml, [quoted Name ü] nvarchar(10), [select] nvarchar(10));
INSERT INTO text_t VALUES
(1, 'ab', 'café', N'bento 🍱', N'日本語 text', 'legacy', N'ünï', N'<a b="1">t</a>', N'tab	inside', N'it''s'),
(2, '', '', N'', N'new
line', '', N'', N'<r/>', N'"quoted"', N'back\slash'),
(3, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL);
CREATE TABLE bin_t (id int NOT NULL PRIMARY KEY, b binary(3), vb varbinary(10), vm varbinary(max), img image, g uniqueidentifier, h hierarchyid);
INSERT INTO bin_t VALUES
(1, 0x00FF10, 0x, 0xC328, 0x02AA, '6F9619FF-8B86-D011-B42D-00C04FC964FF', '/1/2/'),
(2, 0x6162, 0xFFFE, 0x, 0x, '00000000-0000-0000-0000-000000000000', '/'),
(3, NULL, NULL, NULL, NULL, NULL, NULL);
CREATE TABLE variant_t (id int NOT NULL PRIMARY KEY, v sql_variant);
INSERT INTO variant_t VALUES (1, CAST(CAST(5 AS int) AS sql_variant)), (2, CAST(CAST(1.50 AS decimal(5,2)) AS sql_variant)),
(3, CAST(N'ab' AS sql_variant)), (4, CAST(CAST('2024-01-02T03:04:05.12' AS datetime2(2)) AS sql_variant)),
(5, CAST(0x0A AS sql_variant)), (6, CAST(CAST(1 AS money) AS sql_variant)), (7, NULL);
