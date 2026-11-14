//! Redis in the window: the key filter, Load more, and a key opened in its own value tab.
#![cfg(feature = "redis")]
use crate::support::{
    Started, click_id, label_of, name, present, right_click, started_custom, wait_real,
};
use dbento_app::explorer::part_id;
use dbento_app::keys::KeyFilter;
use dbento_app::schema_tree::{CatalogRow, RowKind};
use dbento_testkit::{RD_SECRET_VAR, RdUser, redis_db};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};

/// The explorer index of the rendered row labelled `aria`.
fn row_ix(cx: &mut TestAppContext, s: &Started, aria: &str) -> Option<usize> {
    (0usize..)
        .map_while(|i| label_of(cx, s.win, ("tree-row", i)))
        .position(|l| l == aria)
}
fn model_rows(cx: &mut TestAppContext, s: &Started) -> Vec<CatalogRow> {
    s.sidebar(cx)
        .read_with(cx, |sb, _| sb.tree().rows("rd", 0, ""))
}
/// The key count of folder row `aria`, once listed.
fn folder_keys(cx: &mut TestAppContext, s: &Started, aria: &str) -> Option<usize> {
    model_rows(cx, s)
        .into_iter()
        .find(|r| r.aria == aria)
        .and_then(|r| match r.kind {
            RowKind::KeyFolder { keys, .. } => Some(keys),
            RowKind::Schema
            | RowKind::Kind(_)
            | RowKind::Relation(_)
            | RowKind::Column { .. }
            | RowKind::Group(_)
            | RowKind::Object { .. }
            | RowKind::Loading
            | RowKind::Failed { .. }
            | RowKind::Truncated
            | RowKind::Empty
            | RowKind::Key(_)
            | RowKind::LoadMore
            | RowKind::Database { .. } => None,
        })
}

#[gpui_kit::test]
fn a_filtered_key_listing_pages_and_a_key_opens_in_its_own_value_tab(cx: &mut TestAppContext) {
    // The database's own runtime, kept entered, since the shell starts the app's bridge.
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let db = rt.block_on(redis_db());
    let n = db.index();
    let many: Vec<String> = (0..600)
        .flat_map(|i| [format!("kw:n:{i:04}"), "v".into()])
        .collect();
    let list: Vec<String> = (0..600).map(|i| i.to_string()).collect();
    rt.block_on(async {
        db.cmd(&["DEL", "kw:l"]).await;
        let mset: Vec<&str> = ["MSET"]
            .into_iter()
            .chain(many.iter().map(String::as_str))
            .collect();
        db.cmd(&mset).await;
        let push: Vec<&str> = ["RPUSH", "kw:l"]
            .into_iter()
            .chain(list.iter().map(String::as_str))
            .collect();
        db.cmd(&push).await;
    });
    let password = db.password(RdUser::Reader).unwrap().to_owned();
    let toml = format!(
        "dbento = 1\n{}",
        db.connection_toml("rd", RdUser::Reader, "read_only = true")
    );
    let s = started_custom(cx, &toml, move |sv| {
        sv.env = std::sync::Arc::new(move |v| (v == RD_SECRET_VAR).then(|| password.clone()))
    });
    let (dbrow, folder, more) = (
        format!("rd.db{n}"),
        format!("rd.db{n}/kw:"),
        format!("rd.db{n}:more"),
    );
    click_id(cx, s.win, part_id("connection-toggle", "rd"));
    wait_real(cx, "the db row", |cx| row_ix(cx, &s, &dbrow).is_some());
    let ix = row_ix(cx, &s, &dbrow).unwrap();

    // Filter keys asks, and the closed db keeps the filter for its opening.
    right_click(cx, &s, ("tree-row", ix));
    click_id(cx, s.win, "tree-menu-filter-keys");
    name(cx, &s, "kw:*");
    let filter = s
        .sidebar(cx)
        .read_with(cx, |sb, _| sb.tree().key_filter("rd", n));
    assert_eq!(filter.pattern.as_deref(), Some("kw:*"));

    // set_key_browser: the db opens on key folders, not relations.
    click_id(cx, s.win, ("tree-toggle", ix));
    wait_real(cx, "the kw: folder", |cx| {
        folder_keys(cx, &s, &folder).is_some()
    });

    // Load more: each click appends a page until the scan ends.
    for _ in 0..10 {
        let loading = format!("rd.db{n}:loading");
        wait_real(cx, "a settled page", |cx| {
            row_ix(cx, &s, &loading).is_none()
        });
        let Some(m) = row_ix(cx, &s, &more) else {
            break;
        };
        click_id(cx, s.win, ("tree-row", m));
    }
    assert_eq!(
        folder_keys(cx, &s, &folder),
        Some(601),
        "every kw: key once"
    );
    let tops: Vec<String> = model_rows(cx, &s)
        .into_iter()
        .filter(|r| r.depth == 1)
        .map(|r| r.aria)
        .collect();
    assert_eq!(
        tops,
        std::slice::from_ref(&folder),
        "the filter lists only kw: and the scan ended"
    );

    // OpenKey: a double click opens a value tab titled by the key and its db.
    let f = row_ix(cx, &s, &folder).unwrap();
    click_id(cx, s.win, ("tree-toggle", f));
    let key = format!("rd.db{n}.kw:l");
    wait_real(cx, "the kw:l row", |cx| row_ix(cx, &s, &key).is_some());
    let k = row_ix(cx, &s, &key).unwrap();
    cx.update_window(s.win, |_, w, cx| w.double_click(("tree-row", k), cx))
        .unwrap();
    let title = |cx: &mut TestAppContext| {
        s.shell
            .read_with(cx, |sh, cx| sh.tab().map(|t| t.read(cx).title()))
    };
    wait_real(cx, "the value tab", |cx| {
        title(cx) == Some(format!("kw:l \u{B7} db {n}"))
    });

    // Key mode: the header replaces the editor and Load more pages the list to its end.
    let head = |cx: &mut TestAppContext| label_of(cx, s.win, "key-header");
    wait_real(cx, "the header", |cx| {
        head(cx).is_some_and(|h| h.starts_with("list \u{B7} 600 elements"))
    });
    assert!(!present(cx, s.win, "editor"), "a key tab hides the editor");
    wait_real(cx, "Load more", |cx| present(cx, s.win, "key-load-more"));
    click_id(cx, s.win, "key-load-more");
    wait_real(cx, "the list's end", |cx| {
        !present(cx, s.win, "key-load-more")
    });
    let rows = s.shell.read_with(cx, |sh, cx| {
        sh.tab().unwrap().read(cx).model().read(cx).results()[0]
            .store
            .rows()
    });
    assert_eq!(rows, 600);

    // The opened key keeps its explorer selection rather than handing it to the connection.
    let selected = s.sidebar(cx).read_with(cx, |sb, _| sb.selected_key());
    assert_eq!(selected, Some(("rd".to_owned(), n, b"kw:l".to_vec())));

    // Clear filter on the db row lists every key again.
    let ix = row_ix(cx, &s, &dbrow).unwrap();
    right_click(cx, &s, ("tree-row", ix));
    click_id(cx, s.win, "tree-menu-clear-key-filter");
    let filter = s
        .sidebar(cx)
        .read_with(cx, |sb, _| sb.tree().key_filter("rd", n));
    assert_eq!(filter, KeyFilter::default());
}

/// The explorer label of the selected rendered row, among the first `upto` rows.
fn selected_row(cx: &mut TestAppContext, s: &Started, upto: usize) -> Option<String> {
    cx.update_window(s.win, |_, w, cx| {
        w.render_frame(cx);
        (0..upto).find_map(|i| {
            let e = w.try_find(("tree-row", i))?;
            (e.selected() == Some(true)).then(|| e.label().map(str::to_owned))?
        })
    })
    .unwrap()
}

// A db whose scan has more pages lists Load more, which the arrow keys reach and Enter runs.
#[gpui_kit::test]
fn the_keyboard_reaches_load_more_and_enter_fetches_the_next_page(cx: &mut TestAppContext) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let db = rt.block_on(redis_db());
    let n = db.index();
    // Enough keys that the first page stops short of the scan's end.
    let many: Vec<String> = (0..3000)
        .flat_map(|i| [format!("kb{i:04}"), "v".into()])
        .collect();
    rt.block_on(async {
        let mset: Vec<&str> = ["MSET"]
            .into_iter()
            .chain(many.iter().map(String::as_str))
            .collect();
        db.cmd(&mset).await;
    });
    let password = db.password(RdUser::Reader).unwrap().to_owned();
    let toml = format!(
        "dbento = 1\n{}",
        db.connection_toml("rd", RdUser::Reader, "read_only = true")
    );
    let s = started_custom(cx, &toml, move |sv| {
        sv.env = std::sync::Arc::new(move |v| (v == RD_SECRET_VAR).then(|| password.clone()))
    });
    let (dbrow, more) = (format!("rd.db{n}"), format!("rd.db{n}:more"));
    click_id(cx, s.win, part_id("connection-toggle", "rd"));
    wait_real(cx, "the db row", |cx| row_ix(cx, &s, &dbrow).is_some());
    let ix = row_ix(cx, &s, &dbrow).unwrap();
    click_id(cx, s.win, ("tree-toggle", ix));
    let keys = |cx: &mut TestAppContext| {
        model_rows(cx, &s)
            .iter()
            .filter(|r| matches!(r.kind, RowKind::Key(_)))
            .count()
    };
    wait_real(cx, "Load more", |cx| {
        model_rows(cx, &s).iter().any(|r| r.aria == more)
    });
    let first = keys(cx);
    click_id(cx, s.win, ("tree-row", ix));
    let press = |cx: &mut TestAppContext, k: &str| {
        cx.update_window(s.win, |_, w, cx| w.press(k, cx)).unwrap();
        cx.run_until_parked();
    };
    // The other dbs list below this one, so Up from the last row walks back to Load more.
    let upto = model_rows(cx, &s).len() + 8;
    press(cx, "end");
    for _ in 0..32 {
        if selected_row(cx, &s, upto).as_deref() == Some(more.as_str()) {
            break;
        }
        press(cx, "up");
    }
    assert_eq!(selected_row(cx, &s, upto), Some(more.clone()));
    press(cx, "enter");
    wait_real(cx, "the next page", |cx| keys(cx) > first);
}
