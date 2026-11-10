use std::collections::HashMap;

use aws_sdk_dynamodb::primitives::Blob;
use aws_sdk_dynamodb::types::AttributeValue;
use serde_json::{Value, json};

use super::{
    Cell, cell, document_literal, document_plain_json, item_to_typed, literal, map_literal,
    map_plain_json, plain_json, to_typed, type_name,
};

const BIG: &str = "12345678901234567890123456789012345678";

fn s(x: &str) -> AttributeValue {
    AttributeValue::S(x.to_owned())
}

fn n(x: &str) -> AttributeValue {
    AttributeValue::N(x.to_owned())
}

fn b(bytes: &[u8]) -> AttributeValue {
    AttributeValue::B(Blob::new(bytes.to_vec()))
}

/// One attribute of each of the ten types.
fn torture() -> HashMap<String, AttributeValue> {
    HashMap::from([
        ("s".to_owned(), s("it's")),
        ("n".to_owned(), n(BIG)),
        ("b".to_owned(), b(&[0, 255, 254])),
        ("bool".to_owned(), AttributeValue::Bool(false)),
        ("null".to_owned(), AttributeValue::Null(true)),
        (
            "m".to_owned(),
            AttributeValue::M(HashMap::from([
                ("z".to_owned(), n("1")),
                ("a".to_owned(), s("x")),
            ])),
        ),
        (
            "l".to_owned(),
            AttributeValue::L(vec![AttributeValue::Null(true), n("-1.5E-130"), s("y")]),
        ),
        (
            "ss".to_owned(),
            AttributeValue::Ss(vec!["x".to_owned(), "y".to_owned()]),
        ),
        (
            "ns".to_owned(),
            AttributeValue::Ns(vec!["1".to_owned(), BIG.to_owned()]),
        ),
        (
            "bs".to_owned(),
            AttributeValue::Bs(vec![Blob::new(vec![0u8]), Blob::new(vec![255u8])]),
        ),
    ])
}

#[test]
fn an_item_becomes_typed_dynamodb_json_with_sorted_keys() {
    let typed = Value::Object(item_to_typed(&torture()));
    assert_eq!(
        typed.to_string(),
        format!(
            concat!(
                r#"{{"b":{{"B":"AP/+"}},"bool":{{"BOOL":false}},"bs":{{"BS":["AA==","/w=="]}},"#,
                r#""l":{{"L":[{{"NULL":true}},{{"N":"-1.5E-130"}},{{"S":"y"}}]}},"#,
                r#""m":{{"M":{{"a":{{"S":"x"}},"z":{{"N":"1"}}}}}},"n":{{"N":"{big}"}},"#,
                r#""ns":{{"NS":["1","{big}"]}},"null":{{"NULL":true}},"s":{{"S":"it's"}},"#,
                r#""ss":{{"SS":["x","y"]}}}}"#
            ),
            big = BIG
        )
    );
}

#[test]
fn each_type_has_its_dynamodb_name() {
    let item = item_to_typed(&torture());
    let names: Vec<(&str, &str)> = item
        .iter()
        .map(|(k, v)| (k.as_str(), type_name(v)))
        .collect();
    assert_eq!(
        names,
        [
            ("b", "B"),
            ("bool", "BOOL"),
            ("bs", "BS"),
            ("l", "L"),
            ("m", "M"),
            ("n", "N"),
            ("ns", "NS"),
            ("null", "NULL"),
            ("s", "S"),
            ("ss", "SS"),
        ]
    );
    assert_eq!(type_name(&json!({"$unknown": true})), "unknown");
}

#[test]
fn cells_keep_numbers_exact_bytes_raw_and_only_null_as_null() {
    let item = item_to_typed(&torture());
    let c = |k: &str| cell(&item[k]);
    assert_eq!(c("s"), Cell::Text("it's".to_owned()));
    assert_eq!(c("n"), Cell::Number(BIG.to_owned()));
    assert_eq!(c("b"), Cell::Bytes(vec![0, 255, 254]));
    assert_eq!(c("bool"), Cell::Text("false".to_owned()));
    assert_eq!(c("null"), Cell::Null);
    assert_eq!(c("m"), Cell::Text("{'a': 'x', 'z': 1}".to_owned()));
    assert_eq!(c("l"), Cell::Text("[NULL, -1.5E-130, 'y']".to_owned()));
    assert_eq!(c("ss"), Cell::Text("<<'x', 'y'>>".to_owned()));
    assert_eq!(c("ns"), Cell::Text(format!("<<1, {BIG}>>")));
    assert_eq!(c("bs"), Cell::Text("<<'AA==', '/w=='>>".to_owned()));
    assert_eq!(
        cell(&json!({"$unknown": true})),
        Cell::Text("(unknown type)".to_owned())
    );
}

#[test]
fn literals_quote_strings_and_nest_compactly() {
    assert_eq!(literal(&to_typed(&s("it's"))), "'it''s'");
    assert_eq!(literal(&to_typed(&b(&[0, 255]))), "'AP8='");
    assert_eq!(literal(&to_typed(&AttributeValue::M(HashMap::new()))), "{}");
    assert_eq!(literal(&to_typed(&AttributeValue::L(Vec::new()))), "[]");
    let nested = AttributeValue::M(HashMap::from([(
        "k".to_owned(),
        AttributeValue::L(vec![AttributeValue::M(HashMap::from([(
            "x".to_owned(),
            AttributeValue::Bool(true),
        )]))]),
    )]));
    assert_eq!(literal(&to_typed(&nested)), "{'k': [{'x': true}]}");
    assert_eq!(literal(&json!({"$unknown": true})), "(unknown type)");
}

#[test]
fn an_item_literal_is_one_line_or_one_attribute_per_line() {
    let item = item_to_typed(&HashMap::from([
        ("pk".to_owned(), s("c1")),
        (
            "tags".to_owned(),
            AttributeValue::Ss(vec!["a".to_owned(), "b".to_owned()]),
        ),
    ]));
    assert_eq!(
        map_literal(&item, false),
        "{'pk': 'c1', 'tags': <<'a', 'b'>>}"
    );
    assert_eq!(
        map_literal(&item, true),
        "{\n  'pk': 'c1',\n  'tags': <<'a', 'b'>>\n}"
    );
    assert_eq!(map_literal(&serde_json::Map::new(), true), "{}");
}

#[test]
fn plain_json_writes_numbers_verbatim_sets_as_arrays_and_binary_as_base64() {
    let item = item_to_typed(&torture());
    assert_eq!(
        map_plain_json(&item),
        format!(
            concat!(
                r#"{{"b":"AP/+","bool":false,"bs":["AA==","/w=="],"l":[null,-1.5E-130,"y"],"#,
                r#""m":{{"a":"x","z":1}},"n":{big},"ns":[1,{big}],"null":null,"s":"it's","ss":["x","y"]}}"#
            ),
            big = BIG
        )
    );
    assert_eq!(
        plain_json(&json!({"$unknown": true})),
        r#"{"$unknown":true}"#
    );
}

#[test]
fn a_number_json_cannot_spell_is_written_as_a_string() {
    for (text, want) in [
        ("1E+3", "1E+3"),
        ("-0", "-0"),
        (".5", "\".5\""),
        ("01", "\"01\""),
        ("1.", "\"1.\""),
        ("NaN", "\"NaN\""),
    ] {
        assert_eq!(plain_json(&json!({ "N": text })), want, "{text}");
    }
}

#[test]
fn a_stored_document_that_is_not_a_json_object_says_why() {
    for bytes in [&b"[1]"[..], b"{\"a\":"] {
        for text in [document_literal(bytes, false), document_plain_json(bytes)] {
            let reason = text
                .strip_prefix("/* undecodable document: ")
                .and_then(|t| t.strip_suffix(" */"))
                .unwrap_or_else(|| panic!("{text}"));
            assert!(!reason.is_empty(), "{text}");
        }
    }
}
