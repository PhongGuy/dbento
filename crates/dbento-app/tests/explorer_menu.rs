use crate::support::{
    TREE, click_id, label_of, name, present, right_click, right_click_empty, started, wait_real,
};
use dbento_app::explorer::{connection_id, folder_id};
use dbento_core::FolderPath;
use gpui_kit::TestAppContext;

fn path(p: &str) -> FolderPath {
    p.parse().unwrap()
}

#[gpui_kit::test]
fn a_folder_menu_offers_folder_actions(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    right_click(cx, &s, folder_id(&path("team")));
    for id in [
        "menu-new-connection",
        "menu-new-folder",
        "menu-rename",
        "menu-delete",
    ] {
        assert!(present(cx, s.win, id), "{id}");
    }
    assert!(present(cx, s.win, ("menu-color", 8usize)));
    assert!(!present(cx, s.win, "menu-edit"));
}

#[gpui_kit::test]
fn new_folder_nests_under_the_clicked_folder(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    right_click(cx, &s, folder_id(&path("team")));
    click_id(cx, s.win, "menu-new-folder");
    name(cx, &s, "eu");
    assert!(
        s.toml().ends_with("\n[folders.\"team/eu\"]\n"),
        "{}",
        s.toml()
    );
    assert!(present(cx, s.win, folder_id(&path("team/eu"))));
}

#[gpui_kit::test]
fn new_folder_from_empty_space_is_top_level(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    right_click_empty(cx, &s);
    assert!(!present(cx, s.win, "menu-rename"));
    click_id(cx, s.win, "menu-new-folder");
    name(cx, &s, "ops");
    assert!(s.toml().ends_with("\n[folders.ops]\n"), "{}", s.toml());
}

#[gpui_kit::test]
fn bad_names_stay_in_the_dialog(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    right_click(cx, &s, folder_id(&path("team")));
    click_id(cx, s.win, "menu-new-folder");
    name(cx, &s, "a/b");
    assert!(
        label_of(cx, s.win, "save-name-error")
            .unwrap()
            .contains('/')
    );
    name(cx, &s, "sub");
    assert!(
        label_of(cx, s.win, "save-name-error")
            .unwrap()
            .contains("already exists")
    );
    assert_eq!(s.toml(), TREE);
}

#[gpui_kit::test]
fn rename_moves_the_subtree(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    right_click(cx, &s, folder_id(&path("team")));
    click_id(cx, s.win, "menu-rename");
    name(cx, &s, "crew");
    let text = s.toml();
    assert!(
        text.contains("[folders.crew]") && text.contains("folder = \"crew/sub\""),
        "{text}"
    );
}

#[gpui_kit::test]
fn a_swatch_recolours_and_none_clears(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    right_click(cx, &s, folder_id(&path("team")));
    click_id(cx, s.win, ("menu-color", 4usize));
    assert!(
        s.toml()
            .starts_with("dbento = 1\n[folders.team]\ncolor = \"green\"\n"),
        "{}",
        s.toml()
    );
    right_click(cx, &s, folder_id(&path("team")));
    click_id(cx, s.win, ("menu-color", 0usize));
    assert!(
        s.toml().starts_with("dbento = 1\n[folders.team]\n\n"),
        "{}",
        s.toml()
    );
}

#[gpui_kit::test]
fn delete_asks_then_moves_contents_up(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    right_click(cx, &s, folder_id(&path("team")));
    click_id(cx, s.win, "menu-delete");
    wait_real(cx, "the question", |cx| present(cx, s.win, "delete-ok"));
    assert!(
        label_of(cx, s.win, "confirm-detail")
            .unwrap()
            .contains("the top level")
    );
    click_id(cx, s.win, "delete-ok");
    let text = s.toml();
    assert!(
        !text.contains("[folders.team]") && text.contains("folder = \"sub\""),
        "{text}"
    );
    assert!(present(cx, s.win, connection_id("lite")));
}
