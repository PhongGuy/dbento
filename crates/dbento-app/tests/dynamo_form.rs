//! The DynamoDB connection form: its keys, the consistent-read checkbox, and clearing on switch.
use crate::support;
use crate::support::present;
use dbento_app::connection_dialog::{ConnectionForm, Field};
use dbento_app::sidebar::engine_icon;
use dbento_app::ui::Icons;
use dbento_core::{ConnectionSpec, Edit, Engine, SecretSource, Workspace, plan_edit};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, ElementId, Entity, TestAppContext};

fn open(
    cx: &mut TestAppContext,
    existing: Option<(&str, ConnectionSpec)>,
) -> (AnyWindowHandle, Entity<ConnectionForm>) {
    support::init(cx);
    let existing = existing.map(|(n, s)| (n.to_owned(), s));
    cx.update(|cx| {
        gpui_kit::open_window(Default::default(), cx, |window, cx| {
            cx.new(|cx| ConnectionForm::new(existing, Vec::new(), false, window, cx))
        })
    })
    .unwrap()
}
fn set(
    cx: &mut TestAppContext,
    win: AnyWindowHandle,
    f: &Entity<ConnectionForm>,
    field: Field,
    text: &str,
) {
    let state = f.read_with(cx, |f, _| f.input(field).clone());
    cx.update_window(win, |_, w, cx| {
        state.update(cx, |s, cx| s.set_value(text.to_owned(), w, cx))
    })
    .unwrap();
    cx.run_until_parked();
}
fn click(cx: &mut TestAppContext, win: AnyWindowHandle, id: impl Into<ElementId>) {
    let id = id.into();
    cx.update_window(win, |_, w, cx| w.click(id, cx)).unwrap();
    cx.run_until_parked();
}
fn radio(cx: &mut TestAppContext, win: AnyWindowHandle, group: &'static str, ix: usize) {
    cx.update_window(win, |_, w, cx| w.within(group).click(ix, cx))
        .unwrap();
    cx.run_until_parked();
}
fn edit(
    cx: &mut TestAppContext,
    f: &Entity<ConnectionForm>,
) -> Result<(String, ConnectionSpec), String> {
    f.read_with(cx, |f, cx| f.edit(cx))
}
fn engine_option(
    cx: &mut TestAppContext,
    win: AnyWindowHandle,
    ix: usize,
) -> (Option<String>, Option<bool>) {
    cx.update_window(win, |_, w, cx| {
        w.render_frame(cx);
        let o = w.within("conn-engine").find(ix);
        (o.label().map(str::to_owned), o.selected())
    })
    .unwrap()
}
fn dy() -> ConnectionSpec {
    let mut s = ConnectionSpec::new(Engine::Dynamodb);
    (s.region, s.endpoint, s.access_key_id, s.consistent_read) = (
        Some("eu-west-1".into()),
        Some("http://127.0.0.1:5000".into()),
        Some("AKIDEXAMPLE".into()),
        Some(true),
    );
    s
}

#[gpui_kit::test]
fn the_ninth_engine_is_dynamodb_with_its_keys_and_a_checkbox(cx: &mut TestAppContext) {
    let (win, f) = open(cx, None);
    assert_eq!(engine_option(cx, win, 8).0.as_deref(), Some("DynamoDB"));
    radio(cx, win, "conn-engine", 8);
    for id in [
        "conn-region",
        "conn-endpoint",
        "conn-access-key-id",
        "conn-consistent-read",
        "conn-read-only-help",
    ] {
        assert!(present(cx, win, id), "{id}");
    }
    for id in [
        "conn-host",
        "conn-port",
        "conn-path",
        "conn-uri",
        "conn-database",
        "conn-user",
        "conn-tls",
    ] {
        assert!(!present(cx, win, id), "{id}");
    }
    set(cx, win, &f, Field::Name, "orders");
    set(cx, win, &f, Field::Region, "eu-west-1");
    set(cx, win, &f, Field::Endpoint, "http://127.0.0.1:5000");
    set(cx, win, &f, Field::AccessKeyId, "AKIDEXAMPLE");
    click(cx, win, "conn-consistent-read");
    assert_eq!(edit(cx, &f), Ok(("orders".into(), dy())));
}

#[gpui_kit::test]
fn a_loaded_dynamodb_connection_selects_it_shows_its_keys_and_returns_unchanged(
    cx: &mut TestAppContext,
) {
    let (win, f) = open(cx, Some(("orders", dy())));
    assert_eq!(engine_option(cx, win, 8).1, Some(true));
    for ix in 0..8 {
        assert_eq!(engine_option(cx, win, ix).1, Some(false), "{ix}");
    }
    let region = f.read_with(cx, |f, _| f.input(Field::Region).clone());
    assert_eq!(
        region.read_with(cx, |s, _| s.value().to_string()),
        "eu-west-1"
    );
    assert_eq!(edit(cx, &f), Ok(("orders".into(), dy())));
}

#[gpui_kit::test]
fn unticking_consistent_reads_drops_the_key(cx: &mut TestAppContext) {
    let (win, f) = open(cx, Some(("orders", dy())));
    click(cx, win, "conn-consistent-read");
    let mut want = dy();
    want.consistent_read = None;
    assert_eq!(edit(cx, &f), Ok(("orders".into(), want)));
}

#[gpui_kit::test]
fn leaving_dynamodb_drops_its_keys_and_coming_back_restores_them(cx: &mut TestAppContext) {
    let (win, f) = open(cx, Some(("orders", dy())));
    radio(cx, win, "conn-engine", 7);
    set(cx, win, &f, Field::Host, "cache.internal");
    let (_, rd) = edit(cx, &f).unwrap();
    assert_eq!(
        (
            rd.engine,
            rd.region,
            rd.endpoint,
            rd.access_key_id,
            rd.consistent_read
        ),
        (Engine::Redis, None, None, None, None)
    );
    radio(cx, win, "conn-engine", 8);
    assert_eq!(edit(cx, &f), Ok(("orders".into(), dy())));
}

#[gpui_kit::test]
fn the_secret_rows_wait_for_an_access_key_id(cx: &mut TestAppContext) {
    let (win, f) = open(cx, None);
    radio(cx, win, "conn-engine", 8);
    assert!(present(cx, win, "conn-secret-from-env"));
    assert!(!present(cx, win, "conn-secret"));
    assert!(!present(cx, win, "conn-password"));
    set(cx, win, &f, Field::AccessKeyId, "AKIDEXAMPLE");
    assert!(present(cx, win, "conn-secret"));
    assert!(present(cx, win, "conn-password"));
    assert!(!present(cx, win, "conn-secret-from-env"));
}

#[gpui_kit::test]
fn an_empty_access_key_id_writes_no_secret_and_the_workspace_takes_it(cx: &mut TestAppContext) {
    let (win, f) = open(cx, None);
    radio(cx, win, "conn-engine", 8);
    set(cx, win, &f, Field::Name, "orders");
    set(cx, win, &f, Field::Region, "eu-west-1");
    set(cx, win, &f, Field::AccessKeyId, "AKIDEXAMPLE");
    set(cx, win, &f, Field::Password, "typed-then-hidden");
    radio(cx, win, "conn-secret", 2);
    set(cx, win, &f, Field::AccessKeyId, "");
    let (name, spec) = edit(cx, &f).unwrap();
    assert_eq!(
        (spec.access_key_id.as_deref(), spec.secret.as_ref()),
        (None, None)
    );
    assert!(f.read_with(cx, |f, cx| f.password(cx)).is_none());
    // The written connection validates and draws every credential from the environment.
    let dir = tempfile::tempdir().unwrap();
    Workspace::init(dir.path()).unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    let plan = plan_edit(
        &ws,
        &Edit::PutConnection {
            previous: None,
            name: name.clone(),
            spec,
        },
    )
    .unwrap();
    let conn = plan.workspace().connection(&name).unwrap();
    assert_eq!(conn.secret_source(), &SecretSource::None);
}

#[gpui_kit::test]
fn clearing_a_loaded_access_key_id_drops_its_secret(cx: &mut TestAppContext) {
    let mut loaded = dy();
    loaded.secret = Some(SecretSource::Env("ORDERS_SECRET".into()));
    let (win, f) = open(cx, Some(("orders", loaded)));
    assert!(present(cx, win, "conn-secret"));
    set(cx, win, &f, Field::AccessKeyId, "");
    let mut want = dy();
    want.access_key_id = None;
    assert_eq!(edit(cx, &f), Ok(("orders".into(), want)));
}

#[test]
fn dynamodb_has_its_own_glyph() {
    assert_eq!(engine_icon(Engine::Dynamodb), Icons::Shapes);
    assert_ne!(engine_icon(Engine::Dynamodb), engine_icon(Engine::Mysql));
}
