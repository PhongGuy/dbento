//! The vendored tiberius on the live server: P5 (exact money), P6 (a UTF-8 collation's
//! text) and P4 (each column's full TDS type).
use futures_util::TryStreamExt;
use tiberius::{Column, ColumnData, ColumnType, FixedLenType, QueryItem, TypeInfo, VarLenType};

/// The cells of the first row of `sql`.
async fn first_row(db: &dbento_testkit::MsDb, sql: &str) -> Vec<ColumnData<'static>> {
    let mut c = db.client().await;
    let row = c
        .simple_query(sql)
        .await
        .unwrap()
        .into_row()
        .await
        .unwrap()
        .expect("a row");
    row.into_iter().collect()
}

#[tokio::test]
async fn money_and_smallmoney_decode_as_exact_scale_four_numerics() {
    dbento_testkit::skip_without_mssql!();
    let db = dbento_testkit::mssql_db().await;
    let cells = first_row(
        &db,
        "SELECT CAST(922337203685477.5807 AS money), CAST(-922337203685477.5808 AS money), \
         CAST(900719925474099.0001 AS money), CAST(214748.3647 AS smallmoney), \
         CAST(-214748.3648 AS smallmoney), CAST(NULL AS money)",
    )
    .await;
    let want: [Option<i128>; 6] = [
        Some(9_223_372_036_854_775_807),
        Some(-9_223_372_036_854_775_808),
        Some(9_007_199_254_740_990_001),
        Some(2_147_483_647),
        Some(-2_147_483_648),
        None,
    ];
    assert_eq!(cells.len(), want.len());
    for (i, (cell, want)) in cells.iter().zip(want).enumerate() {
        match cell {
            ColumnData::Numeric(n) => assert_eq!(
                n.map(|n| (n.value(), n.scale())),
                want.map(|v| (v, 4)),
                "cell {i}"
            ),
            other => panic!("cell {i}: {other:?}"),
        }
    }
}

#[tokio::test]
async fn a_utf8_collated_varchar_inside_sql_variant_decodes_as_utf8() {
    dbento_testkit::skip_without_mssql!();
    let db = dbento_testkit::mssql_db().await;
    let cells = first_row(
        &db,
        "SELECT CAST(CAST(N'bento 弁当 🍱 Ωμέγα' COLLATE Latin1_General_100_CI_AS_SC_UTF8 \
         AS varchar(100)) AS sql_variant), \
         CAST(CAST(N'æøå' COLLATE Latin1_General_CI_AS AS varchar(10)) AS sql_variant)",
    )
    .await;
    match (&cells[0], &cells[1]) {
        (ColumnData::String(Some(u8)), ColumnData::String(Some(l1))) => {
            assert_eq!(u8, "bento 弁当 🍱 Ωμέγα");
            assert_eq!(l1, "æøå");
        }
        other => panic!("{other:?}"),
    }
}

/// The columns of the first result set of `sql`, as the server described them.
async fn columns(db: &dbento_testkit::MsDb, sql: &str) -> Vec<Column> {
    let mut c = db.client().await;
    let mut stream = c.simple_query(sql).await.unwrap();
    while let Some(item) = stream.try_next().await.unwrap() {
        if let QueryItem::Metadata(m) = item {
            return m.columns().to_vec();
        }
    }
    panic!("no result set: {sql}");
}

#[tokio::test]
async fn result_columns_carry_their_size_precision_scale_udt_and_user_type() {
    dbento_testkit::skip_without_mssql!();
    let db = dbento_testkit::mssql_db().await;
    let mut c = db.client().await;
    c.simple_query(
        "CREATE TABLE dbo.ty (n nvarchar(20) NULL, m nvarchar(max) NULL, d decimal(10,2) NULL, \
         nu numeric(38,0) NULL, t2 datetime2(3) NULL, tm time(0) NULL, h hierarchyid NULL, \
         rv rowversion, i int NOT NULL, b binary(8) NULL)",
    )
    .await
    .unwrap()
    .into_results()
    .await
    .unwrap();
    drop(c);
    let cols = columns(&db, "SELECT n, m, d, nu, t2, tm, h, rv, i, b FROM dbo.ty").await;
    let info: Vec<&TypeInfo> = cols
        .iter()
        .map(|c| {
            c.type_info()
                .unwrap_or_else(|| panic!("{} has no type", c.name()))
        })
        .collect();
    match info[0] {
        TypeInfo::VarLenSized(cx) => {
            assert_eq!((cx.r#type(), cx.len()), (VarLenType::NVarchar, 40));
            assert!(cx.collation().is_some());
        }
        other => panic!("n: {other:?}"),
    }
    match info[1] {
        TypeInfo::VarLenSized(cx) => {
            assert_eq!((cx.r#type(), cx.len()), (VarLenType::NVarchar, 0xFFFF))
        }
        other => panic!("m: {other:?}"),
    }
    match (info[2], info[3]) {
        (
            TypeInfo::VarLenSizedPrecision {
                ty: a,
                precision: ap,
                scale: asc,
                ..
            },
            TypeInfo::VarLenSizedPrecision {
                ty: b,
                precision: bp,
                scale: bsc,
                ..
            },
        ) => {
            assert_eq!((*a, *ap, *asc), (VarLenType::Decimaln, 10, 2));
            assert_eq!((*b, *bp, *bsc), (VarLenType::Numericn, 38, 0));
        }
        other => panic!("d, nu: {other:?}"),
    }
    match (info[4], info[5]) {
        (TypeInfo::VarLenSized(a), TypeInfo::VarLenSized(b)) => {
            assert_eq!((a.r#type(), a.len()), (VarLenType::Datetime2, 3));
            assert_eq!((b.r#type(), b.len()), (VarLenType::Timen, 0));
        }
        other => panic!("t2, tm: {other:?}"),
    }
    match info[6] {
        TypeInfo::Udt(u) => assert_eq!(
            (u.schema_name.as_str(), u.type_name.as_str()),
            ("sys", "hierarchyid")
        ),
        other => panic!("h: {other:?}"),
    }
    for ix in [7, 9] {
        match info[ix] {
            TypeInfo::VarLenSized(cx) => {
                assert_eq!((cx.r#type(), cx.len()), (VarLenType::BigBinary, 8), "{ix}")
            }
            other => panic!("{ix}: {other:?}"),
        }
    }
    assert!(
        matches!(info[8], TypeInfo::FixedLen(FixedLenType::Int4)),
        "{:?}",
        info[8]
    );
    let users: Vec<u32> = cols.iter().map(Column::user_type).collect();
    assert_eq!(users[7], 80, "rowversion: {users:?}");
    assert_ne!(users[9], 80, "binary(8): {users:?}");
    let made = Column::new("x".into(), ColumnType::Int4);
    assert!(made.type_info().is_none());
    assert_eq!(made.user_type(), 0);
}
