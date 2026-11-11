//! DynamoDB's names in the tab: grammar, read-only badge and error code.
use dbento_app::query_tab::{
    READ_ONLY_DYNAMODB_TOOLTIP, editor_language, read_only_badge_text, read_only_tooltip,
    read_only_tooltip_of,
};
use dbento_app::result_body::{ErrorView, code_label};
use dbento_core::{Engine, ReadOnlyEnforcement, read_only_residual};

#[test]
fn partiql_is_highlighted_as_sql() {
    assert_eq!(editor_language(Engine::Dynamodb), "sql");
}

#[test]
fn the_badge_says_best_effort_and_the_residual_at_every_level() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("dbento.toml"),
        "dbento = 1\n[connections.dy]\nengine = \"dynamodb\"\nregion = \"eu-west-1\"\n\
         access_key_id = \"AKIDEXAMPLE\"\nsecret = { env = \"X\" }\nread_only = true\n",
    )
    .unwrap();
    let ws = dbento_core::Workspace::open(dir.path()).unwrap();
    let c = ws.declared("dy").unwrap();
    let residual = read_only_residual(Engine::Dynamodb).expect("DynamoDB states a residual");
    let want = format!("{READ_ONLY_DYNAMODB_TOOLTIP} {residual}");
    for level in [None, Some(ReadOnlyEnforcement::BestEffort)] {
        assert_eq!(read_only_badge_text(c, level), want, "{level:?}");
    }
    assert_eq!(
        read_only_tooltip(Engine::Dynamodb),
        READ_ONLY_DYNAMODB_TOOLTIP
    );
    assert_eq!(read_only_tooltip_of(c), READ_ONLY_DYNAMODB_TOOLTIP);
    assert_ne!(read_only_tooltip(Engine::Dynamodb), "Read-only connection.");
}

#[test]
fn the_error_code_shows_alone() {
    let v = ErrorView {
        message: "Statement wasn't well formed".into(),
        code: Some("ValidationException".into()),
        sqlstate: None,
        detail: None,
        hint: None,
        position: None,
        sql: "SELEC 1".into(),
    };
    assert_eq!(
        code_label(Some(Engine::Dynamodb), &v).as_deref(),
        Some("ValidationException")
    );
}
