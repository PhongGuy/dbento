use crate::support::{Cell, run, sqlite_fixture, text};
use dbento_core::{Limits, SessionOptions};

#[tokio::test]
async fn a_column_of_mixed_kinds_renders_each_row_on_its_own() {
    let fx = sqlite_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
    let sql = "WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r WHERE x < 2000) \
               SELECT x, CASE x % 5 WHEN 0 THEN x WHEN 1 THEN x * 0.5 WHEN 2 THEN 't' || x \
               WHEN 3 THEN x'ab' ELSE NULL END FROM r";
    let ran = run(&mut s, sql, Limits::default()).await.remove(0).unwrap();
    let expected: Vec<Vec<Cell>> = (1..=2000i64)
        .map(|x| {
            let second = match x % 5 {
                0 => text(&x.to_string()),
                1 => text(&format!("{:?}", x as f64 * 0.5)),
                2 => text(&format!("t{x}")),
                3 => Cell::Bytes(vec![0xab]),
                _ => Cell::Null,
            };
            vec![text(&x.to_string()), second]
        })
        .collect();
    assert_eq!(ran.rows, expected);
}
