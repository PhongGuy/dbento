use super::*;
use crate::introspect::RelationKind;
use crate::objects::TriggerState;

fn obj(fields: &[(&str, Wire)]) -> Wire {
    Wire::Object(
        fields
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect(),
    )
}
fn text(s: &str) -> Wire {
    Wire::Text(s.to_owned())
}
fn list(items: Vec<Wire>) -> Wire {
    Wire::Array(items)
}
fn table(name: &str, schemafull: bool, view: Option<&str>) -> Wire {
    let mut fields = vec![
        ("kind", obj(&[("kind", text("NORMAL"))])),
        ("name", text(name)),
        ("schemafull", Wire::Bool(schemafull)),
    ];
    if let Some(v) = view {
        fields.push(("view", text(v)));
    }
    obj(&fields)
}
fn db(tables: Vec<Wire>) -> Wire {
    obj(&[("tables", list(tables)), ("users", list(vec![]))])
}
fn field(name: &str, extra: &[(&str, Wire)]) -> Wire {
    let mut fields = vec![("name", text(name)), ("table", text("person"))];
    fields.extend(extra.iter().cloned());
    obj(&fields)
}
fn structure(fields: Vec<Wire>) -> Wire {
    obj(&[
        ("events", list(vec![])),
        ("fields", list(fields)),
        ("indexes", list(vec![])),
        ("tables", list(vec![])),
    ])
}
fn unexpected(e: IntrospectError) -> String {
    match e {
        IntrospectError::Unexpected { message } => message,
        other => panic!("not Unexpected: {other:?}"),
    }
}
fn names(columns: &[ColumnMeta]) -> Vec<&str> {
    columns.iter().map(ColumnMeta::name).collect()
}

#[test]
fn schemas_are_the_namespaces_databases_in_name_order() {
    let ns = obj(&[
        ("accesses", obj(&[])),
        (
            "databases",
            obj(&[
                ("b", text("DEFINE DATABASE b")),
                ("a", text("DEFINE DATABASE a")),
            ]),
        ),
        ("users", obj(&[])),
    ]);
    let got: Vec<String> = schema_names(&ns)
        .unwrap()
        .iter()
        .map(|s| s.name().to_owned())
        .collect();
    assert_eq!(got, ["a", "b"]);
    let m = unexpected(schema_names(&obj(&[("users", obj(&[]))])).unwrap_err());
    assert!(m.contains("`databases`"), "{m}");
}

#[test]
fn relations_are_tables_and_views_in_name_order() {
    let reply = db(vec![
        table("person", true, None),
        table("adults", false, Some("AS SELECT name FROM person")),
        table("knows", true, None),
    ]);
    let listed = relation_list(&reply).unwrap();
    let got: Vec<_> = listed
        .items()
        .iter()
        .map(|r| (r.name(), r.kind()))
        .collect();
    assert_eq!(
        got,
        [
            ("adults", RelationKind::View),
            ("knows", RelationKind::Table),
            ("person", RelationKind::Table)
        ]
    );
    assert!(!listed.truncated());
    let m = unexpected(relation_list(&obj(&[("users", list(vec![]))])).unwrap_err());
    assert!(m.contains("`tables`"), "{m}");
    let nameless = db(vec![obj(&[("schemafull", Wire::Bool(true))])]);
    assert!(unexpected(relation_list(&nameless).unwrap_err()).contains("`name`"));
}

#[test]
fn a_tables_entry_says_whether_it_is_schemafull_or_a_view() {
    let reply = db(vec![
        table("person", true, None),
        table("adults", false, Some("AS SELECT 1")),
    ]);
    let full = TableInfo {
        schemafull: true,
        view: false,
    };
    assert_eq!(table_info(&reply, "person").unwrap(), Some(full));
    let view = TableInfo {
        schemafull: false,
        view: true,
    };
    assert_eq!(table_info(&reply, "adults").unwrap(), Some(view));
    assert_eq!(table_info(&reply, "nosuch").unwrap(), None);
    let bare = db(vec![obj(&[("name", text("t"))])]);
    assert!(unexpected(table_info(&bare, "t").unwrap_err()).contains("`schemafull`"));
}

#[test]
fn declared_columns_put_id_first_and_read_kind_default_and_nullability() {
    let reply = structure(vec![
        field(
            "addr",
            &[("flexible", Wire::Bool(true)), ("kind", text("object"))],
        ),
        field("addr.city", &[("kind", text("string"))]),
        field(
            "age",
            &[("default", text("18")), ("kind", text("none | int"))],
        ),
        field("name", &[("kind", text("string"))]),
        field("nick", &[("kind", text("option<string>"))]),
        field("note", &[("kind", text("string | null"))]),
        field("pts", &[("computed", text("1 + 1"))]),
    ]);
    let cols = declared_columns(&reply, "person").unwrap();
    assert_eq!(
        names(&cols),
        [
            "id",
            "addr",
            "addr.city",
            "age",
            "name",
            "nick",
            "note",
            "pts"
        ]
    );
    let id = &cols[0];
    assert_eq!(
        (id.type_name(), id.primary_key(), id.nullable()),
        ("record<person>", Some(1), false)
    );
    let age = &cols[3];
    assert_eq!(
        (age.type_name(), age.nullable(), age.default()),
        ("none | int", true, Some("18"))
    );
    assert_eq!((cols[4].nullable(), cols[4].primary_key()), (false, None));
    assert!(cols[5].nullable() && cols[6].nullable());
    assert_eq!((cols[7].type_name(), cols[7].nullable()), ("any", true));
    assert!(cols.iter().all(|c| !c.inferred()));
    assert!(
        !nullable("array<int | none>"),
        "a nested none is the element's, not the field's"
    );
}

#[test]
fn a_declared_id_keeps_its_kind_and_stays_first() {
    let reply = structure(vec![
        field("a", &[("kind", text("int"))]),
        field("id", &[("kind", text("int"))]),
    ]);
    let cols = declared_columns(&reply, "person").unwrap();
    assert_eq!(names(&cols), ["id", "a"]);
    assert_eq!(
        (cols[0].type_name(), cols[0].primary_key()),
        ("int", Some(1))
    );
    let m = unexpected(declared_columns(&obj(&[("indexes", list(vec![]))]), "person").unwrap_err());
    assert!(m.contains("`fields`"), "{m}");
}

#[test]
fn sampled_keys_not_declared_are_appended_as_inferred() {
    let declared = declared_columns(
        &structure(vec![field("name", &[("kind", text("string"))])]),
        "loose",
    )
    .unwrap();
    let id = || Wire::Record {
        table: "loose".to_owned(),
        key: Box::new(Wire::Int(1)),
    };
    let rows = [
        obj(&[("a", Wire::Int(1)), ("id", id()), ("name", text("x"))]),
        obj(&[("b", text("y")), ("id", id())]),
    ];
    let cols = with_inferred(declared.clone(), &rows);
    assert_eq!(names(&cols), ["id", "name", "a", "b"]);
    assert!(!cols[0].inferred() && !cols[1].inferred());
    assert!(cols[2].inferred() && cols[2].nullable());
    assert_eq!(
        (cols[2].type_name(), cols[3].type_name()),
        ("int", "string")
    );
    assert_eq!(with_inferred(declared.clone(), &[]), declared);
    let mixed = [obj(&[("a", Wire::Int(1))]), Wire::Int(2)];
    assert_eq!(with_inferred(declared.clone(), &mixed), declared);
}

#[test]
fn indexes_and_events_are_the_relation_objects() {
    let rel = RelationRef::new("app", "person");
    let index = |name: &str, cols: &[&str], kind: &str| {
        obj(&[
            ("cols", list(cols.iter().map(|c| text(c)).collect())),
            ("index", text(kind)),
            ("name", text(name)),
            ("table", text("person")),
        ])
    };
    let reply = obj(&[
        (
            "events",
            list(vec![obj(&[
                ("name", text("ev")),
                ("then", list(vec![text("{ CREATE audit }")])),
                ("what", Wire::Table("person".to_owned())),
                ("when", text("$event = 'CREATE'")),
            ])]),
        ),
        ("fields", list(vec![])),
        (
            "indexes",
            list(vec![
                index("ix_age", &["age"], ""),
                index("ix_c", &["name", "age"], ""),
                index("ix_ft", &["name"], "FULLTEXT ANALYZER an BM25(1.2,0.75)"),
                index("ix_name", &["name"], "UNIQUE"),
            ]),
        ),
    ]);
    let objects = relation_objects(&rel, &reply).unwrap();
    let got: Vec<_> = objects
        .indexes()
        .iter()
        .map(|i| {
            (
                i.name(),
                i.columns().to_vec(),
                i.unique(),
                i.primary(),
                i.partial(),
                i.method(),
            )
        })
        .collect();
    let s = |v: &[&str]| v.iter().map(|c| (*c).to_owned()).collect::<Vec<_>>();
    assert_eq!(
        got,
        [
            ("ix_age", s(&["age"]), false, false, false, None),
            ("ix_c", s(&["name", "age"]), false, false, false, None),
            (
                "ix_ft",
                s(&["name"]),
                false,
                false,
                false,
                Some("FULLTEXT ANALYZER an BM25(1.2,0.75)")
            ),
            ("ix_name", s(&["name"]), true, false, false, None),
        ]
    );
    let triggers: Vec<_> = objects
        .triggers()
        .iter()
        .map(|t| (t.name(), t.state()))
        .collect();
    assert_eq!(triggers, [("ev", TriggerState::Enabled)]);
    assert!(objects.constraints().is_empty());
    let m = unexpected(relation_objects(&rel, &obj(&[("events", list(vec![]))])).unwrap_err());
    assert!(m.contains("`indexes`"), "{m}");
}

#[test]
fn a_failed_statement_carries_its_kind_and_the_servers_message() {
    let reply = list(vec![
        obj(&[("result", list(vec![])), ("status", text("OK"))]),
        obj(&[
            ("kind", text("NotAllowed")),
            ("result", text("Not enough permissions")),
            ("status", text("ERR")),
        ]),
    ]);
    assert!(matches!(
        statement(&reply, 0).unwrap(),
        Statement::Ok(Wire::Array(_))
    ));
    let Statement::Err(e) = statement(&reply, 1).unwrap() else {
        panic!("statement 2 failed")
    };
    assert_eq!(
        (e.kind.as_str(), e.message.as_str()),
        ("NotAllowed", "Not enough permissions")
    );
    assert!(unexpected(statement(&reply, 2).err().unwrap()).contains("statement 3"));
}
