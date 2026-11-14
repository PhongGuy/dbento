//! The lasting chip a table tab shows on an engine that cannot edit rows yet.
#![cfg(all(feature = "mongodb", feature = "dynamodb"))]
use dbento_app::browse::not_editable_text;

fn workspace() -> (tempfile::TempDir, dbento_core::Workspace) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("dbento.toml"),
        "dbento = 1\n\
         [connections.my]\nengine = \"mysql\"\nhost = \"h\"\nuser = \"u\"\n\
         [connections.maria]\nengine = \"mariadb\"\nhost = \"h\"\nuser = \"u\"\n\
         [connections.mg]\nengine = \"mongodb\"\nuri = \"mongodb://h/shop\"\n\
         [connections.dy]\nengine = \"dynamodb\"\nregion = \"eu-west-1\"\n\
         access_key_id = \"AKIDEXAMPLE\"\nsecret = { env = \"X\" }\n\
         [connections.pg]\nengine = \"postgres\"\nhost = \"h\"\nuser = \"u\"\ndatabase = \"d\"\n\
         [connections.lite]\nengine = \"sqlite\"\npath = \"app.db\"\n",
    )
    .unwrap();
    let ws = dbento_core::Workspace::open(dir.path()).unwrap();
    (dir, ws)
}

#[test]
fn the_chip_names_the_engine_as_the_pickers_write_it() {
    let (_dir, ws) = workspace();
    for (name, engine) in [
        ("my", "MySQL"),
        ("maria", "MariaDB"),
        ("mg", "MongoDB"),
        ("dy", "DynamoDB"),
    ] {
        assert_eq!(
            not_editable_text(ws.declared(name).unwrap()),
            Some(format!(
                "Read-only: editing is not available on {engine} yet"
            )),
            "{name}"
        );
    }
}

#[test]
fn an_engine_that_edits_rows_has_no_chip() {
    let (_dir, ws) = workspace();
    for name in ["pg", "lite"] {
        assert_eq!(
            not_editable_text(ws.declared(name).unwrap()),
            None,
            "{name}"
        );
    }
}
