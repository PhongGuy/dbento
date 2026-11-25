//! Every MySQL type the editor binds: inserted, read as the grid reads it, written back
//! unchanged, and used as a key.
use crate::support::{Cell, edit_value, my_fixtures, pairs, rejected, row_key, text, val};
use dbento_core::{Change, ChangeOutcome, EditValue};
use dbento_testkit::MyFlavor;

const TYPES: &str = "CREATE TABLE ty (id INT PRIMARY KEY, dec30 DECIMAL(30,10), ubig BIGINT UNSIGNED, \
  tiny TINYINT(1), e ENUM('x','y','z'), st SET('a','b','c'), j JSON, d DATE, dt6 DATETIME(6), \
  ts TIMESTAMP(6) NULL, y YEAR, tm TIME, f FLOAT, dbl DOUBLE, ch CHAR(3), vc VARCHAR(20), tx TEXT, \
  vb VARBINARY(300), bl BLOB, b64 BIT(64), b8 BIT(8), g GEOMETRY) ENGINE=InnoDB";
const POINT: &str = "000000000101000000000000000000F03F0000000000000040";

fn unhex(h: &str) -> Vec<u8> {
    (0..h.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn every_type_reads_back_and_writes_back_unchanged() {
    for (label, fx) in my_fixtures("").await {
        fx.exec(TYPES).await;
        let target = fx.target("ty").await;
        let all: Vec<u8> = (0..=255).collect();
        let values = vec![
            ("id", val("1")),
            ("dec30", val("12345678901234567890.0123456789")),
            ("ubig", val("18446744073709551615")),
            ("tiny", val("1")),
            ("e", val("y")),
            ("st", val("a,b")),
            ("j", val("{\"a\": [1, 2.50, \"x\"], \"b\": null}")),
            ("d", val("2024-02-29")),
            ("dt6", val("2024-02-29 23:59:59.123456")),
            ("ts", val("2024-01-02 03:04:05.6")),
            ("y", val("2024")),
            ("tm", val("-838:59:59")),
            ("f", val("1.1")),
            ("dbl", val("0.1")),
            ("ch", val("ab")),
            ("vc", val("it's \\ \u{e9}\u{1f600}")),
            ("tx", val("line\nbreak")),
            ("vb", EditValue::Bytes(all.clone())),
            ("bl", EditValue::Bytes(vec![0, 0x27, 0x5c, 0xff])),
            ("b64", EditValue::Bytes(vec![0xff; 8])),
            ("b8", EditValue::Bytes(vec![5])),
            ("g", EditValue::Bytes(unhex(POINT))),
        ];
        let insert = Change::Insert {
            values: values
                .iter()
                .map(|(c, v)| ((*c).to_owned(), v.clone()))
                .collect(),
        };
        fx.apply_changes(&target, vec![insert])
            .await
            .unwrap()
            .commit()
            .await
            .unwrap();
        let json = match fx.db.flavor() {
            MyFlavor::Mysql => "{\"a\": [1, 2.5, \"x\"], \"b\": null}",
            MyFlavor::Mariadb => "{\"a\": [1, 2.50, \"x\"], \"b\": null}",
        };
        let want = vec![
            text("1"),
            text("12345678901234567890.0123456789"),
            text("18446744073709551615"),
            text("1"),
            text("y"),
            text("a,b"),
            text(json),
            text("2024-02-29"),
            text("2024-02-29 23:59:59.123456"),
            text("2024-01-02 03:04:05.600000"),
            text("2024"),
            text("-838:59:59"),
            text("1.1"),
            text("0.1"),
            text("ab"),
            text("it's \\ \u{e9}\u{1f600}"),
            text("line\nbreak"),
            Cell::Bytes(all),
            Cell::Bytes(vec![0, 0x27, 0x5c, 0xff]),
            Cell::Bytes(vec![0xff; 8]),
            Cell::Bytes(vec![5]),
            Cell::Bytes(unhex(POINT)),
        ];
        let read = fx.row("SELECT * FROM ty").await;
        assert_eq!(read, want, "{label}");
        let names: Vec<&str> = values.iter().map(|(c, _)| *c).collect();
        let set: Vec<(String, EditValue)> = names
            .iter()
            .zip(&read)
            .skip(1)
            .map(|(c, cell)| ((*c).to_owned(), edit_value(cell)))
            .collect();
        let back = Change::Update {
            key: row_key(&[("id", val("1"))]),
            set,
            expected: vec![],
        };
        let applied = fx.apply_changes(&target, vec![back]).await.unwrap();
        assert_eq!(applied.affected(), 1, "{label}");
        applied.commit().await.unwrap();
        assert_eq!(
            fx.row("SELECT * FROM ty").await,
            want,
            "{label}: written back unchanged"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn each_key_type_matches_its_own_browse_text() {
    let keys = [
        ("INT", "-5"),
        ("BIGINT UNSIGNED", "18446744073709551615"),
        ("DECIMAL(30,10)", "1.5"),
        ("DOUBLE", "0.1"),
        ("DATE", "'2024-02-29'"),
        ("DATETIME(6)", "'2024-02-29 23:59:59.123456'"),
        ("TIMESTAMP(6)", "'2024-01-02 03:04:05.600000'"),
        ("TIME", "'-838:59:59'"),
        ("YEAR", "2024"),
        ("CHAR(3)", "'ab'"),
        ("VARCHAR(20)", "'it''s \\\\ \u{e9}'"),
        ("VARBINARY(16)", "X'00FF27'"),
        ("BINARY(4)", "X'00010203'"),
        ("ENUM('x','y')", "'y'"),
        ("SET('a','b')", "'a,b'"),
    ];
    for (label, fx) in my_fixtures("").await {
        for (i, (ty, lit)) in keys.iter().enumerate() {
            fx.exec(&format!(
                "CREATE TABLE k{i} (k {ty} PRIMARY KEY, n INT); INSERT INTO k{i} VALUES ({lit}, 1)"
            ))
            .await;
            let cell = fx.row(&format!("SELECT k FROM k{i}")).await.remove(0);
            let target = fx.target(&format!("k{i}")).await;
            let change = Change::Update {
                key: row_key(&[("k", edit_value(&cell))]),
                set: pairs(&[("n", val("2"))]),
                expected: vec![],
            };
            let applied = fx.apply_changes(&target, vec![change]).await;
            assert!(applied.is_ok(), "{label} {ty}: {cell:?} {applied:?}");
            applied.unwrap().commit().await.unwrap();
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_zero_date_is_refused_by_mysql_and_kept_by_mariadb() {
    for (label, fx) in my_fixtures("").await {
        fx.exec(
            "CREATE TABLE zd (k INT PRIMARY KEY, d DATE); INSERT INTO zd VALUES (1, '2024-01-01');",
        )
        .await;
        let target = fx.target("zd").await;
        let change = Change::Update {
            key: row_key(&[("k", val("1"))]),
            set: pairs(&[("d", val("0000-00-00"))]),
            expected: vec![],
        };
        let r = fx.apply_changes(&target, vec![change]).await;
        match fx.db.flavor() {
            MyFlavor::Mysql => match rejected(r).as_slice() {
                [ChangeOutcome::Failed(m)] => {
                    assert!(m.contains("Incorrect date value"), "{label}: {m}")
                }
                other => panic!("{label}: {other:?}"),
            },
            MyFlavor::Mariadb => r.unwrap().commit().await.unwrap(),
        }
    }
}
