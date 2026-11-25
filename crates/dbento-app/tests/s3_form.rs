//! The S3 connection form: its keys, the buckets list, the virtual-hosted checkbox, flags
//! kept apart per key, clearing on switch, and its glyph.
use crate::support;
use crate::support::present;
use dbento_app::connection_dialog::{ConnectionForm, FLAG_FIELDS, Field};
use dbento_app::sidebar::engine_icon;
use dbento_app::ui::Icons;
use dbento_core::{ConnectionSpec, Engine, FieldKind};
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
fn shown(cx: &mut TestAppContext, f: &Entity<ConnectionForm>, field: Field) -> String {
    let state = f.read_with(cx, |f, _| f.input(field).clone());
    state.read_with(cx, |s, _| s.value().to_string())
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
fn s3() -> ConnectionSpec {
    let mut s = ConnectionSpec::new(Engine::S3);
    (
        s.region,
        s.endpoint,
        s.access_key_id,
        s.virtual_hosted,
        s.buckets,
    ) = (
        Some("auto".into()),
        Some("https://acct.r2.cloudflarestorage.com".into()),
        Some("AKIDEXAMPLE".into()),
        Some(true),
        Some(vec!["logs".into(), "media".into(), "archive".into()]),
    );
    s
}

#[gpui_kit::test]
fn the_tenth_engine_is_s3_with_its_keys_buckets_and_a_checkbox(cx: &mut TestAppContext) {
    let (win, f) = open(cx, None);
    assert_eq!(engine_option(cx, win, 9).0.as_deref(), Some("S3"));
    radio(cx, win, "conn-engine", 9);
    for id in [
        "conn-region",
        "conn-endpoint",
        "conn-access-key-id",
        "conn-virtual-hosted",
        "conn-buckets",
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
        "conn-consistent-read",
    ] {
        assert!(!present(cx, win, id), "{id}");
    }
    set(cx, win, &f, Field::Name, "assets");
    set(cx, win, &f, Field::Region, "auto");
    set(
        cx,
        win,
        &f,
        Field::Endpoint,
        "https://acct.r2.cloudflarestorage.com",
    );
    set(cx, win, &f, Field::AccessKeyId, "AKIDEXAMPLE");
    set(cx, win, &f, Field::Buckets, " logs, media  archive ,");
    click(cx, win, "conn-virtual-hosted");
    assert_eq!(edit(cx, &f), Ok(("assets".into(), s3())));
}

#[gpui_kit::test]
fn a_loaded_s3_connection_shows_its_buckets_and_returns_unchanged(cx: &mut TestAppContext) {
    let (win, f) = open(cx, Some(("assets", s3())));
    assert_eq!(engine_option(cx, win, 9).1, Some(true));
    assert_eq!(shown(cx, &f, Field::Buckets), "logs, media, archive");
    assert_eq!(edit(cx, &f), Ok(("assets".into(), s3())));
}

#[gpui_kit::test]
fn clearing_buckets_and_unticking_virtual_hosted_drop_both_keys(cx: &mut TestAppContext) {
    let (win, f) = open(cx, Some(("assets", s3())));
    set(cx, win, &f, Field::Buckets, " , ");
    click(cx, win, "conn-virtual-hosted");
    let mut want = s3();
    (want.buckets, want.virtual_hosted) = (None, None);
    assert_eq!(edit(cx, &f), Ok(("assets".into(), want)));
}

#[gpui_kit::test]
fn dynamodb_keeps_its_flag_apart_from_s3s(cx: &mut TestAppContext) {
    let mut dy = ConnectionSpec::new(Engine::Dynamodb);
    (dy.region, dy.endpoint, dy.access_key_id) = (
        Some("eu-west-1".into()),
        Some("http://127.0.0.1:5000".into()),
        Some("AKIDEXAMPLE".into()),
    );
    let (win, f) = open(cx, Some(("orders", dy)));
    click(cx, win, "conn-consistent-read");
    let (_, got) = edit(cx, &f).unwrap();
    assert_eq!(
        (got.consistent_read, got.virtual_hosted),
        (Some(true), None),
        "only the ticked key is written"
    );
}

#[gpui_kit::test]
fn leaving_s3_drops_its_keys_and_coming_back_restores_them(cx: &mut TestAppContext) {
    let (win, f) = open(cx, Some(("assets", s3())));
    radio(cx, win, "conn-engine", 8);
    let (_, dy) = edit(cx, &f).unwrap();
    assert_eq!(
        (dy.engine, dy.virtual_hosted, dy.buckets),
        (Engine::Dynamodb, None, None),
        "DynamoDB has neither S3 key"
    );
    radio(cx, win, "conn-engine", 9);
    assert_eq!(edit(cx, &f), Ok(("assets".into(), s3())));
}

#[test]
fn s3_has_its_own_glyph() {
    assert_eq!(engine_icon(Engine::S3), Icons::Archive);
    assert_ne!(
        engine_icon(Engine::S3),
        Icons::FolderOpen,
        "folders are prefixes"
    );
    assert!(
        Icons::ALL.contains(&Icons::Archive),
        "the registry test loads it"
    );
}

#[test]
fn every_descriptor_flag_is_one_the_form_saves() {
    let engines = [
        Engine::Postgres,
        Engine::Sqlite,
        Engine::Mysql,
        Engine::Duckdb,
        Engine::Mssql,
        Engine::Surrealdb,
        Engine::Mongodb,
        Engine::Redis,
        Engine::Dynamodb,
        Engine::S3,
    ];
    for engine in engines {
        let form = dbento_core::connection_form(engine);
        for f in form.sections.iter().flat_map(|s| &s.fields) {
            if f.kind == FieldKind::Flag {
                assert!(
                    FLAG_FIELDS.contains(&f.key),
                    "{engine:?} flag {} has a checkbox that never saves",
                    f.key
                );
            }
        }
    }
}
