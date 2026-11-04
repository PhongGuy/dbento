//! Every SQL Server type family renders as the Values table says; nothing becomes NULL.
mod support;

use dbento_core::{Limits, Session, SessionOptions};
use support::{Cell, Ran, ms_off, run, text};

/// The one result of `sql`.
async fn one(s: &mut Session, sql: &str) -> Ran {
    run(s, sql, Limits::default()).await.remove(0).unwrap()
}

fn types(ran: &Ran) -> Vec<&str> {
    ran.columns.iter().map(|(_, t)| t.as_str()).collect()
}

fn bytes(b: &[u8]) -> Cell {
    Cell::Bytes(b.to_vec())
}

#[tokio::test]
async fn ints_bits_and_text_render_with_their_declared_types() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    let mut s = fx.session(SessionOptions::default()).await;
    let ran = one(
        &mut s,
        "SELECT N'x' AS t, CAST(5 AS bigint) AS n, CAST(NULL AS int) AS z, CAST(1 AS bit) AS f, \
         CAST(7 AS tinyint) AS ti, CAST(-3 AS smallint) AS si, 'abc' AS v",
    )
    .await;
    assert_eq!(
        types(&ran),
        [
            "NVARCHAR(1)",
            "BIGINT",
            "INT",
            "BIT",
            "TINYINT",
            "SMALLINT",
            "VARCHAR(3)"
        ]
    );
    assert_eq!(
        ran.rows,
        vec![vec![
            text("x"),
            text("5"),
            Cell::Null,
            text("1"),
            text("7"),
            text("-3"),
            text("abc")
        ]]
    );
}

#[tokio::test]
async fn exact_numbers_keep_their_scale_and_floats_their_shortest_digits() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    let mut s = fx.session(SessionOptions::default()).await;
    let ran = one(
        &mut s,
        "SELECT CAST(-99999999.99 AS decimal(10,2)) AS d, CAST(0.05 AS decimal(5,4)) AS d4, \
         CAST(99999999999999999999999999999999999999 AS numeric(38,0)) AS n38, \
         CAST(922337203685477.5807 AS money) AS m, CAST(-214748.3648 AS smallmoney) AS sm, \
         CAST(1 AS money) AS m1, CAST(1e20 AS float) AS f, CAST(0.1 AS real) AS r, \
         CAST(1e-5 AS float) AS small, CAST(NULL AS decimal(5,1)) AS dn, CAST(NULL AS money) AS mn",
    )
    .await;
    assert_eq!(
        types(&ran),
        [
            "DECIMAL(10,2)",
            "DECIMAL(5,4)",
            "NUMERIC(38,0)",
            "MONEY",
            "SMALLMONEY",
            "MONEY",
            "FLOAT",
            "REAL",
            "FLOAT",
            "DECIMAL(5,1)",
            "MONEY"
        ]
    );
    let want = [
        "-99999999.99",
        "0.0500",
        "99999999999999999999999999999999999999",
        "922337203685477.5807",
        "-214748.3648",
        "1.0000",
        "1e+20",
        "0.1",
        "1e-05",
    ];
    let mut row: Vec<Cell> = want.map(text).to_vec();
    row.extend([Cell::Null, Cell::Null]);
    assert_eq!(ran.rows, vec![row]);
}

/// Each date and time value beside the server's own style-121 text of it.
#[tokio::test]
async fn dates_and_times_read_as_the_server_prints_them_in_style_121() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    let mut s = fx.session(SessionOptions::default()).await;
    let values = [
        "CAST('2024-01-02T03:04:05.123' AS datetime)",
        "CAST('1753-01-01T23:59:59.998' AS datetime)",
        "CAST('2024-01-02T03:04:05.002' AS datetime)",
        "CAST('2024-01-02T03:04:29' AS smalldatetime)",
        "CAST('0001-01-01' AS date)",
        "CAST('9999-12-31' AS date)",
        "CAST('23:59:59.1234567' AS time(7))",
        "CAST('01:02:03' AS time(0))",
        "CAST('9999-12-31T23:59:59.999' AS datetime2(3))",
        "CAST('2024-01-02T03:04:05.1234567-05:30' AS datetimeoffset(7))",
        "CAST('2024-01-02T23:30:00+14:00' AS datetimeoffset(0))",
    ];
    let sql = format!(
        "SELECT {}",
        values
            .iter()
            .map(|v| format!("{v}, CONVERT(nvarchar(40), {v}, 121)"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let ran = one(&mut s, &sql).await;
    let shown: Vec<&str> = types(&ran).into_iter().step_by(2).collect();
    assert_eq!(
        shown,
        [
            "DATETIME",
            "DATETIME",
            "DATETIME",
            "SMALLDATETIME",
            "DATE",
            "DATE",
            "TIME(7)",
            "TIME(0)",
            "DATETIME2(3)",
            "DATETIMEOFFSET(7)",
            "DATETIMEOFFSET(0)"
        ]
    );
    let row = &ran.rows[0];
    for (i, v) in values.iter().enumerate() {
        assert!(matches!(row[2 * i], Cell::Text(_)), "{v}: {:?}", row[2 * i]);
        assert_eq!(row[2 * i], row[2 * i + 1], "{v}");
    }
    assert_eq!(row[0], text("2024-01-02 03:04:05.123"));
    assert_eq!(row[6], text("2024-01-02 03:04:00.000"));
    assert_eq!(row[18], text("2024-01-02 03:04:05.1234567 -05:30"));
}

#[tokio::test]
async fn text_bytes_guids_xml_and_clr_values_render_by_family() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    let mut s = fx.session(SessionOptions::default()).await;
    let ran = one(
        &mut s,
        "SELECT CAST('abc' AS char(5)) AS c, N'弁当' AS nv, CAST(N'x' AS nvarchar(max)) AS nm, \
         CAST('t' AS text) AS tx, CAST(N'nt' AS ntext) AS ntx, CAST('<a>1</a>' AS xml) AS x, \
         CAST('6f9619ff-8b86-d011-b42d-00c04fc964ff' AS uniqueidentifier) AS g, \
         CAST(0x00FF10 AS varbinary(10)) AS vb, CAST(0x01 AS binary(2)) AS b2, \
         CAST(0xC0FFEE AS image) AS img, hierarchyid::Parse('/1/2/') AS h, \
         CAST(NULL AS varbinary(1)) AS vn",
    )
    .await;
    assert_eq!(
        types(&ran),
        [
            "CHAR(5)",
            "NVARCHAR(2)",
            "NVARCHAR(MAX)",
            "TEXT",
            "NTEXT",
            "XML",
            "UNIQUEIDENTIFIER",
            "VARBINARY(10)",
            "BINARY(2)",
            "IMAGE",
            "HIERARCHYID",
            "VARBINARY(1)"
        ]
    );
    assert_eq!(
        ran.rows,
        vec![vec![
            text("abc  "),
            text("弁当"),
            text("x"),
            text("t"),
            text("nt"),
            text("<a>1</a>"),
            text("6F9619FF-8B86-D011-B42D-00C04FC964FF"),
            bytes(&[0x00, 0xFF, 0x10]),
            bytes(&[0x01, 0x00]),
            bytes(&[0xC0, 0xFF, 0xEE]),
            bytes(&[0x5B, 0x40]),
            Cell::Null,
        ]]
    );
}

#[tokio::test]
async fn a_rowversion_is_its_eight_bytes() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    let mut s = fx.session(SessionOptions::default()).await;
    one(&mut s, "CREATE TABLE #rv (id int, rv rowversion)").await;
    one(&mut s, "INSERT INTO #rv (id) VALUES (1)").await;
    let ran = one(
        &mut s,
        "SELECT rv, CAST(rv AS varbinary(8)) AS raw FROM #rv",
    )
    .await;
    assert_eq!(types(&ran), ["ROWVERSION", "VARBINARY(8)"]);
    let row = &ran.rows[0];
    assert!(matches!(&row[0], Cell::Bytes(b) if b.len() == 8), "{row:?}");
    assert_eq!(row[0], row[1]);
}

#[tokio::test]
async fn sql_variant_cells_take_their_base_types_text() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    let mut s = fx.session(SessionOptions::default()).await;
    let ran = one(
        &mut s,
        "SELECT CAST(CAST(5 AS int) AS sql_variant) AS i, \
         CAST(CAST(1.50 AS decimal(5,2)) AS sql_variant) AS d, CAST(N'ab' AS sql_variant) AS s, \
         CAST(CAST('2024-01-02T03:04:05.12' AS datetime2(2)) AS sql_variant) AS t, \
         CAST(CAST(N'bento 弁当 🍱' COLLATE Latin1_General_100_CI_AS_SC_UTF8 AS varchar(100)) \
         AS sql_variant) AS u8, CAST(0x0A AS sql_variant) AS b, \
         CAST(CAST(1 AS money) AS sql_variant) AS m, CAST(NULL AS sql_variant) AS n",
    )
    .await;
    assert!(
        types(&ran).iter().all(|t| *t == "SQL_VARIANT"),
        "{:?}",
        types(&ran)
    );
    assert_eq!(
        ran.rows,
        vec![vec![
            text("5"),
            text("1.50"),
            text("ab"),
            text("2024-01-02 03:04:05.12"),
            text("bento 弁当 🍱"),
            bytes(&[0x0A]),
            text("1.0000"),
            Cell::Null,
        ]]
    );
}

// Nullable table columns go over the wire as INTN, BITN and FLTN; each renders as its
// fixed-width type does, and a NULL in it stays NULL.
#[tokio::test]
async fn nullable_int_bit_and_float_columns_render_like_their_fixed_width_types() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    let mut s = fx.session(SessionOptions::default()).await;
    let setup = [
        "CREATE TABLE #n (ti tinyint NULL, si smallint NULL, i int NULL, bi bigint NULL, \
         b bit NULL, r real NULL, f float NULL)",
        "INSERT INTO #n VALUES (200, -300, 70000, 9000000000, 1, 1.5, 0.1)",
        "INSERT INTO #n VALUES (NULL, NULL, NULL, NULL, NULL, NULL, NULL)",
    ];
    for sql in setup {
        run(&mut s, sql, Limits::default()).await.remove(0).unwrap();
    }
    let ran = run(
        &mut s,
        "SELECT * FROM #n ORDER BY i DESC",
        Limits::default(),
    )
    .await
    .remove(0)
    .unwrap();
    let types: Vec<&str> = ran.columns.iter().map(|(_, t)| t.as_str()).collect();
    assert_eq!(
        types,
        [
            "TINYINT", "SMALLINT", "INT", "BIGINT", "BIT", "REAL", "FLOAT"
        ]
    );
    let values = ["200", "-300", "70000", "9000000000", "1", "1.5", "0.1"];
    assert_eq!(
        ran.rows,
        vec![values.map(text).to_vec(), vec![Cell::Null; 7]]
    );
}
