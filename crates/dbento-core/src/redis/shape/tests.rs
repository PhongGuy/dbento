use ::redis::parse_redis_value;

use super::{ShapeError, Shaped, reply_to_rows};
use crate::value::CellRef;

const COMMAND: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/redis/valkey-9.0.7-command.resp3"
));
const DOCS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/redis/valkey-9.0.7-command-docs.resp3"
));

/// A cell as the grid sees it: null, text, number text or bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
enum C {
    Null,
    T(String),
    N(String),
    B(Vec<u8>),
}

fn t(s: &str) -> C {
    C::T(s.to_owned())
}
fn n(s: &str) -> C {
    C::N(s.to_owned())
}
fn col(name: &str, ty: &str) -> (String, String) {
    (name.to_owned(), ty.to_owned())
}
fn shape(bytes: &[u8], max_rows: Option<u64>) -> Shaped {
    reply_to_rows(parse_redis_value(bytes).unwrap(), max_rows).unwrap()
}
fn columns(s: &Shaped) -> Vec<(String, String)> {
    s.columns
        .iter()
        .map(|c| (c.name().to_owned(), c.type_name().to_owned()))
        .collect()
}

fn rows(s: &Shaped) -> Vec<Vec<C>> {
    let mut out = Vec::new();
    for b in &s.batches {
        for r in 0..b.len() {
            let row = (0..b.column_count())
                .map(|c| match b.cell(r, c) {
                    CellRef::Null => C::Null,
                    CellRef::Text(x) if b.is_number(r, c) => C::N(x.to_owned()),
                    CellRef::Text(x) => C::T(x.to_owned()),
                    CellRef::Bytes(x) => C::B(x.to_vec()),
                })
                .collect();
            out.push(row);
        }
    }
    out
}

fn ints(count: usize) -> Vec<u8> {
    let mut b = format!("*{count}\r\n").into_bytes();
    for i in 0..count {
        b.extend(format!(":{i}\r\n").bytes());
    }
    b
}

#[test]
fn a_scalar_is_one_value_cell_typed_by_its_kind() {
    let cases: [(&[u8], &str, C); 11] = [
        (b"$5\r\nhello\r\n", "string", t("hello")),
        (b"+PONG\r\n", "string", t("PONG")),
        (b"=15\r\ntxt:Some string\r\n", "string", t("Some string")),
        (b":-42\r\n", "integer", n("-42")),
        (b",3.5\r\n", "double", n("3.5")),
        (b",-inf\r\n", "double", n("-inf")),
        (
            b"(123456789012345678901234567890\r\n",
            "bignumber",
            n("123456789012345678901234567890"),
        ),
        (b"#t\r\n", "boolean", t("true")),
        (b"$2\r\n\xff\x00\r\n", "bytes", C::B(vec![0xff, 0])),
        (b"_\r\n", "", C::Null),
        (b"$-1\r\n", "", C::Null),
    ];
    for (wire, ty, cell) in cases {
        let s = shape(wire, None);
        assert_eq!(columns(&s), [col("value", ty)], "{wire:?}");
        assert_eq!(
            (rows(&s), s.rows, s.truncated),
            (vec![vec![cell]], 1, false),
            "{wire:?}"
        );
    }
}

#[test]
fn ok_has_no_columns_and_no_rows() {
    let s = shape(b"+OK\r\n", None);
    assert!(s.columns.is_empty() && rows(&s).is_empty());
    assert_eq!((s.rows, s.truncated), (0, false));
}

#[test]
fn a_flat_array_or_set_is_one_value_column() {
    let s = shape(b"*3\r\n$1\r\na\r\n:2\r\n_\r\n", None);
    assert_eq!(columns(&s), [col("value", "mixed")]);
    assert_eq!(rows(&s), [vec![t("a")], vec![n("2")], vec![C::Null]]);
    let s = shape(b"~2\r\n$1\r\na\r\n$1\r\nb\r\n", None);
    assert_eq!(columns(&s), [col("value", "string")]);
    assert_eq!(rows(&s), [vec![t("a")], vec![t("b")]]);
    let s = shape(b"*0\r\n", None);
    assert_eq!(columns(&s), [col("value", "")]);
    assert!(rows(&s).is_empty());
}

#[test]
fn rows_of_equal_length_arrays_become_numbered_columns_up_to_sixteen() {
    let s = shape(
        b"*2\r\n*2\r\n$1\r\na\r\n,1.5\r\n*2\r\n$1\r\nb\r\n,2.5\r\n",
        None,
    );
    assert_eq!(columns(&s), [col("1", "string"), col("2", "double")]);
    assert_eq!(rows(&s), [vec![t("a"), n("1.5")], vec![t("b"), n("2.5")]]);
    let wide = |width: usize| [b"*1\r\n".as_slice(), ints(width).as_slice()].concat();
    let s = shape(&wide(16), None);
    let names: Vec<String> = s.columns.iter().map(|c| c.name().to_owned()).collect();
    assert_eq!(names, (1..=16).map(|i| i.to_string()).collect::<Vec<_>>());
    let s = shape(&wide(17), None);
    let json = format!(
        "[{}]",
        (0..17).map(|i| i.to_string()).collect::<Vec<_>>().join(",")
    );
    assert_eq!(columns(&s), [col("value", "array")]);
    assert_eq!(rows(&s), [vec![t(&json)]]);
    let s = shape(b"*2\r\n*1\r\n:1\r\n*2\r\n:1\r\n:2\r\n", None);
    assert_eq!(columns(&s), [col("value", "array")]);
    assert_eq!(rows(&s), [vec![t("[1]")], vec![t("[1,2]")]]);
}

#[test]
fn a_map_is_key_and_value_rows() {
    let s = shape(b"%2\r\n$1\r\nf\r\n$1\r\nv\r\n$1\r\ng\r\n:7\r\n", None);
    assert_eq!(columns(&s), [col("key", "string"), col("value", "mixed")]);
    assert_eq!(rows(&s), [vec![t("f"), t("v")], vec![t("g"), n("7")]]);
}

#[test]
fn nested_aggregates_are_compact_json() {
    let s = shape(
        b"*2\r\n%3\r\n+a\r\n*2\r\n$1\r\n\xfe\r\n_\r\n+b\r\n,inf\r\n:5\r\n#f\r\n\
          ~3\r\n(12345678901234567890\r\n,1.5\r\n=7\r\ntxt:abc\r\n",
        None,
    );
    assert_eq!(columns(&s), [col("value", "mixed")]);
    assert_eq!(
        rows(&s),
        [
            vec![t(r#"{"a":[{"$bytes":"/g=="},null],"b":"inf","5":false}"#)],
            vec![t(r#"["12345678901234567890",1.5,"abc"]"#)],
        ]
    );
}

#[test]
fn an_error_inside_a_reply_is_its_text_and_ok_inside_is_ok() {
    let s = shape(b"*2\r\n+OK\r\n-WRONGTYPE Operation against a key\r\n", None);
    assert_eq!(columns(&s), [col("value", "mixed")]);
    assert_eq!(
        rows(&s),
        [vec![t("OK")], vec![t("WRONGTYPE Operation against a key")]]
    );
    let s = shape(b"*1\r\n%1\r\n+e\r\n-ERR bad\r\n", None);
    assert_eq!(columns(&s), [col("value", "map")]);
    assert_eq!(rows(&s), [vec![t(r#"{"e":{"$error":"ERR bad"}}"#)]]);
}

#[test]
fn a_top_level_error_or_push_is_not_rows() {
    let reply = |b: &[u8]| reply_to_rows(parse_redis_value(b).unwrap(), None).err();
    assert_eq!(
        reply(b"-NOPERM User x has no permissions\r\n"),
        Some(ShapeError::Server {
            code: "NOPERM".into(),
            message: "User x has no permissions".into(),
        })
    );
    assert_eq!(reply(b">2\r\n+message\r\n+x\r\n"), Some(ShapeError::Push));
}

#[test]
fn an_attribute_gives_its_data() {
    let s = shape(b"|1\r\n+ttl\r\n:3\r\n$1\r\nv\r\n", None);
    assert_eq!(columns(&s), [col("value", "string")]);
    assert_eq!(rows(&s), [vec![t("v")]]);
}

#[test]
fn the_row_limit_stops_turning_elements_into_rows() {
    let s = shape(&ints(5), Some(3));
    assert_eq!(rows(&s), [vec![n("0")], vec![n("1")], vec![n("2")]]);
    assert_eq!((s.rows, s.truncated), (3, true));
    for limit in [Some(5), None] {
        let s = shape(&ints(5), limit);
        assert_eq!((s.rows, s.truncated), (5, false), "{limit:?}");
    }
    let s = shape(b"%2\r\n$1\r\nf\r\n$1\r\nv\r\n$1\r\ng\r\n:7\r\n", Some(1));
    assert_eq!((rows(&s), s.truncated), (vec![vec![t("f"), t("v")]], true));
    let s = shape(b":1\r\n", Some(0));
    assert_eq!(columns(&s), [col("value", "")]);
    assert_eq!((s.rows, s.truncated), (0, true));
}

#[test]
fn a_long_reply_spans_batches() {
    let s = shape(&ints(3000), None);
    assert_eq!((s.rows, s.batches.len()), (3000, 3));
    assert_eq!(rows(&s)[2999], [n("2999")]);
}

#[test]
fn the_valkey_command_table_reads_as_ten_columns() {
    let s = shape(COMMAND, Some(100));
    let types = [
        "string", "integer", "array", "integer", "integer", "integer", "array", "array", "array",
        "array",
    ];
    let want: Vec<_> = types
        .iter()
        .enumerate()
        .map(|(i, ty)| col(&(i + 1).to_string(), ty))
        .collect();
    assert_eq!(columns(&s), want);
    assert_eq!((s.rows, s.truncated), (100, true));
    let first = &rows(&s)[0];
    assert_eq!(first[0], t("keys"));
    assert_eq!(first[1], n("2"));
    assert_eq!(first[2], t(r#"["readonly"]"#));
    assert_eq!(first[6], t(r#"["@keyspace","@read","@slow","@dangerous"]"#));
}

#[test]
fn the_valkey_docs_reply_is_a_map_of_maps() {
    let s = shape(DOCS, None);
    assert_eq!(columns(&s), [col("key", "string"), col("value", "map")]);
    assert_eq!((s.rows, s.truncated), (254, false));
    assert!(matches!(&rows(&s)[0][1], C::T(json) if json.starts_with('{')));
}

#[test]
fn a_bare_error_code_displays_as_the_code_alone() {
    let e = reply_to_rows(parse_redis_value(b"-NOSCRIPT\r\n").unwrap(), None).unwrap_err();
    assert_eq!(e.to_string(), "NOSCRIPT");
    let full = reply_to_rows(parse_redis_value(b"-ERR bad\r\n").unwrap(), None).unwrap_err();
    assert_eq!(full.to_string(), "ERR bad");
}
