//! S3 objects in the window: an object tab with its header and modes, downloads through the
//! save dialog, and the window close naming a running download.
#![cfg(feature = "s3")]
use std::sync::Arc;

use crate::support::{Started, click_id, label_of, present, started_custom, wait_real};
use dbento_app::downloads::DownloadState;
use dbento_app::object_view::PreviewMode;
use dbento_testkit::{S3_SECRET, S3_SECRET_VAR, S3Moto, s3_moto};
use gpui_kit::{AppContext as _, TestAppContext, VisualTestContext};

fn window(cx: &mut TestAppContext, toml: &str) -> Started {
    started_custom(cx, toml, |sv| {
        sv.env = Arc::new(|v| (v == S3_SECRET_VAR).then(|| S3_SECRET.to_owned()))
    })
}
fn s3_window(cx: &mut TestAppContext, s3: &S3Moto) -> Started {
    window(cx, &format!("dbento = 1\n{}", s3.connection_toml("s3", "")))
}
fn open(cx: &mut TestAppContext, s: &Started, bucket: &str, key: &str) {
    let (shell, bucket, key) = (s.shell.clone(), bucket.to_owned(), key.to_owned());
    cx.update_window(s.win, |_, w, cx| {
        shell.update(cx, |sh, cx| sh.open_object(0, bucket, key, w, cx))
    })
    .unwrap();
}
fn view_mode(cx: &mut TestAppContext, s: &Started) -> Option<PreviewMode> {
    s.shell.read_with(cx, |sh, cx| {
        sh.tab().and_then(|t| {
            t.read(cx)
                .model()
                .read(cx)
                .object_view()
                .and_then(|v| v.mode())
        })
    })
}

#[gpui_kit::test]
fn an_object_opens_in_its_own_tab_with_its_header_and_modes(cx: &mut TestAppContext) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let s3 = rt.block_on(s3_moto());
    let s = s3_window(cx, &s3);
    open(cx, &s, s3.bucket(), "json/doc.json");
    wait_real(cx, "the preview", |cx| present(cx, s.win, "object-text"));
    let header = label_of(cx, s.win, "object-header").unwrap();
    assert!(
        header.starts_with(&format!(
            "s3://{}/json/doc.json \u{b7} 26 bytes",
            s3.bucket()
        )),
        "{header}"
    );
    assert_eq!(view_mode(cx, &s), Some(PreviewMode::Json));
    for id in [
        "object-mode-text",
        "object-mode-json",
        "object-mode-hex",
        "object-download",
        "object-copy-key",
        "object-copy-url",
    ] {
        assert!(present(cx, s.win, id), "{id}");
    }
    click_id(cx, s.win, "object-mode-hex");
    assert!(present(cx, s.win, ("object-hex-row", 0usize)));
    assert!(!present(cx, s.win, "object-text"));
    click_id(cx, s.win, "object-copy-url");
    let clip = cx.read_from_clipboard().and_then(|c| c.text());
    assert_eq!(clip, Some(format!("s3://{}/json/doc.json", s3.bucket())));
}

#[gpui_kit::test]
fn a_missing_object_shows_its_key_escaped(cx: &mut TestAppContext) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let s3 = rt.block_on(s3_moto());
    let s = s3_window(cx, &s3);
    open(cx, &s, s3.bucket(), "hostile/esc\u{1b}[31mgone");
    wait_real(cx, "the error", |cx| present(cx, s.win, "object-error"));
    let e = label_of(cx, s.win, "object-error").unwrap();
    assert!(e.contains("\\u{1b}") && !e.contains('\u{1b}'), "{e}");
    let title = s
        .shell
        .read_with(cx, |sh, cx| sh.tab().unwrap().read(cx).title());
    assert!(!title.contains('\u{1b}'), "{title}");
}

#[gpui_kit::test]
fn download_asks_for_a_file_lands_it_and_replaces_only_what_was_chosen(cx: &mut TestAppContext) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let s3 = rt.block_on(s3_moto());
    let s = s3_window(cx, &s3);
    let out = tempfile::tempdir().unwrap();
    let first = tempfile::tempdir().unwrap();
    let start = first.path().to_path_buf();
    s.shell
        .update(cx, |sh, _| sh.set_download_folder(start.clone()));
    let dest = out.path().join("hello.txt");
    open(cx, &s, s3.bucket(), "text/hello.txt");
    wait_real(cx, "the preview", |cx| {
        present(cx, s.win, "object-download")
    });
    click_id(cx, s.win, "object-download");
    let target = dest.clone();
    cx.simulate_new_path_selection(move |dir| {
        assert_eq!(
            dir,
            start.as_path(),
            "the first dialog opens in the set folder"
        );
        Some(target)
    });
    wait_real(cx, "the file", |_| dest.exists());
    let d = s.shell.read_with(cx, |sh, _| sh.downloads().clone());
    wait_real(cx, "the strip", |cx| {
        present(cx, s.win, ("download-dismiss", 0usize))
    });
    assert_eq!(
        d.read_with(cx, |d, _| d.state(0)),
        Some(DownloadState::Done { bytes: 13 })
    );
    std::fs::write(&dest, b"old").unwrap();
    click_id(cx, s.win, "object-download");
    let (folder, target) = (out.path().to_path_buf(), dest.clone());
    cx.simulate_new_path_selection(move |dir| {
        assert_eq!(
            dir,
            folder.as_path(),
            "the dialog opens where the last download went"
        );
        Some(target)
    });
    wait_real(cx, "the replace", |_| {
        std::fs::read(&dest).unwrap() == b"hello, bento\n"
    });
}

#[gpui_kit::test]
fn the_window_close_names_a_running_download_and_cancels_it(cx: &mut TestAppContext) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let toml = format!(
        "dbento = 1\n[connections.stall]\nengine = \"s3\"\nregion = \"us-east-1\"\nendpoint = \"http://127.0.0.1:{port}\"\naccess_key_id = \"AKIDSTALL\"\nsecret = {{ env = \"{S3_SECRET_VAR}\" }}\nbuckets = [\"b\"]\n"
    );
    let s = window(cx, &toml);
    let out = tempfile::tempdir().unwrap();
    let shell = s.shell.clone();
    cx.update_window(s.win, |_, w, cx| {
        shell.update(cx, |sh, cx| {
            sh.download_object("stall", dbento_core::ObjectKey::new("b", "k.txt"), w, cx)
        })
    })
    .unwrap();
    let (target, home) = (out.path().join("k.txt"), s.files.path().to_path_buf());
    cx.simulate_new_path_selection(move |dir| {
        assert_eq!(
            dir,
            home.as_path(),
            "a test window never opens a user folder"
        );
        Some(target)
    });
    wait_real(cx, "the running download", |cx| {
        present(cx, s.win, ("download-cancel", 0usize))
    });
    let mut v = VisualTestContext::from_window(s.win, cx);
    assert!(!v.simulate_close(), "the close waits for the answer");
    wait_real(cx, "the question", |cx| {
        present(cx, s.win, "confirm-dialog")
    });
    let detail = label_of(cx, s.win, "confirm-detail").unwrap();
    assert!(
        detail.contains("Closing Dbento cancels the download of k.txt."),
        "{detail}"
    );
    click_id(cx, s.win, "discard-close");
    wait_real(cx, "the window to close", |cx| {
        cx.update(|cx| cx.windows().is_empty())
    });
    assert!(
        std::fs::read_dir(out.path()).unwrap().next().is_none(),
        "no file and no partial file"
    );
    drop(listener);
}

fn stall_toml(port: u16) -> String {
    format!(
        "dbento = 1\n[connections.stall]\nengine = \"s3\"\nregion = \"us-east-1\"\nendpoint = \"http://127.0.0.1:{port}\"\naccess_key_id = \"AKIDSTALL\"\nsecret = {{ env = \"{S3_SECRET_VAR}\" }}\nbuckets = [\"b\"]\n"
    )
}

#[gpui_kit::test]
fn download_on_an_undeclared_connection_says_why(cx: &mut TestAppContext) {
    let s = window(cx, &stall_toml(9));
    let shell = s.shell.clone();
    cx.update_window(s.win, |_, w, cx| {
        shell.update(cx, |sh, cx| {
            sh.download_object("gone", dbento_core::ObjectKey::new("b", "k.txt"), w, cx)
        })
    })
    .unwrap();
    cx.run_until_parked();
    let notice = label_of(cx, s.win, "notice");
    assert_eq!(
        notice.as_deref(),
        Some("Cannot download k.txt: connection gone is not in this workspace."),
    );
}

#[gpui_kit::test]
fn an_object_refused_by_a_running_tab_leaves_its_title(cx: &mut TestAppContext) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let s = window(cx, &stall_toml(port));
    let shell = s.shell.clone();
    cx.update_window(s.win, |_, w, cx| {
        shell.update(cx, |sh, cx| sh.open_sql("stall", "ls 's3://b/'", w, cx))
    })
    .unwrap();
    let tab = s.shell.read_with(cx, |sh, _| sh.tab().unwrap().clone());
    tab.update(cx, |t, cx| t.run_all(cx));
    assert!(
        tab.read_with(cx, |t, cx| t.model().read(cx).running()),
        "the run holds the tab"
    );
    let before = tab.read_with(cx, |t, _| t.title());
    tab.update(cx, |t, cx| t.open_object("b".into(), "k.txt".into(), cx));
    assert_eq!(tab.read_with(cx, |t, _| t.title()), before);
    assert!(
        tab.read_with(cx, |t, cx| t.model().read(cx).object_view().is_none()),
        "no object"
    );
    drop(listener);
}
