//! Downloads in the app: each runs on its own session, reports progress, refuses to clobber
//! unless replacing, and a cancel reaches only the download it was pressed for.
#![cfg(feature = "s3")]
use std::path::{Path, PathBuf};

use crate::support::{self, click_id, init_tokio, present, s3_spec, wait_real};
use dbento_app::downloads::{
    DownloadRequest, DownloadState, Downloads, download_dir, download_error_text, downloads_strip,
    progress_text, replace_existing,
};
use dbento_app::runner::RunnerSpec;
use dbento_core::{DownloadError, ObjectBrowseError, ObjectKey, Workspace};
use dbento_testkit::{S3_SECRET, S3_SECRET_VAR, THREE_MIB, big_byte, s3_moto};
use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, TestAppContext,
    Window, div,
};

struct Strip(Entity<Downloads>);
impl Render for Strip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().children(downloads_strip(&self.0, cx))
    }
}
fn state(cx: &mut TestAppContext, d: &Entity<Downloads>, id: usize) -> Option<DownloadState> {
    d.read_with(cx, |d, _| d.state(id))
}
fn ended(cx: &mut TestAppContext, d: &Entity<Downloads>, id: usize) -> bool {
    !matches!(
        state(cx, d, id),
        Some(DownloadState::Connecting | DownloadState::Running) | None
    )
}
fn start(cx: &mut TestAppContext, d: &Entity<Downloads>, req: DownloadRequest) -> usize {
    d.update(cx, |d, cx| d.start(req, cx))
}
/// A connection whose endpoint is a loopback listener that never answers, so its connect stalls.
fn stall_spec(port: u16) -> (tempfile::TempDir, RunnerSpec) {
    let dir = tempfile::tempdir().unwrap();
    let toml = format!(
        "dbento = 1\n[connections.stall]\nengine = \"s3\"\nregion = \"us-east-1\"\nendpoint = \"http://127.0.0.1:{port}\"\naccess_key_id = \"AKIDSTALL\"\nsecret = {{ env = \"{S3_SECRET_VAR}\" }}\nbuckets = [\"b\"]\n"
    );
    std::fs::write(dir.path().join("dbento.toml"), toml).unwrap();
    let spec = RunnerSpec {
        workspace: std::sync::Arc::new(Workspace::open(dir.path()).unwrap()),
        connection: "stall".into(),
        store: std::sync::Arc::new(dbento_core::MemoryStore::new()),
        env: std::sync::Arc::new(|v| (v == S3_SECRET_VAR).then(|| S3_SECRET.to_owned())),
        ssh: dbento_app::ssh::SshContext::none(),
    };
    (dir, spec)
}
fn files_in(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect()
}

#[gpui_kit::test]
fn a_download_lands_byte_exact_with_its_progress(cx: &mut TestAppContext) {
    support::init(cx);
    init_tokio(cx);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let s3 = rt.block_on(s3_moto());
    let (_ws, spec) = s3_spec(&s3, "");
    let out = tempfile::tempdir().unwrap();
    let dest = out.path().join("big.bin");
    let d = cx.new(|_| Downloads::new());
    let id = start(
        cx,
        &d,
        DownloadRequest::new(
            spec,
            ObjectKey::new(s3.bucket(), "big/three-mib.bin"),
            dest.clone(),
        ),
    );
    wait_real(cx, "the download", |cx| ended(cx, &d, id));
    let big = THREE_MIB as u64;
    assert_eq!(state(cx, &d, id), Some(DownloadState::Done { bytes: big }));
    assert_eq!(
        d.read_with(cx, |d, _| d.progress(id)),
        Some((big, Some(big)))
    );
    let got = std::fs::read(&dest).unwrap();
    assert!(
        got.len() == THREE_MIB && got.iter().enumerate().all(|(i, b)| *b == big_byte(i)),
        "byte-exact"
    );
    assert!(d.read_with(cx, |d, _| d.running()).is_empty());
}

#[gpui_kit::test]
fn cancel_reaches_only_the_download_it_was_pressed_for(cx: &mut TestAppContext) {
    support::init(cx);
    init_tokio(cx);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let s3 = rt.block_on(s3_moto());
    let (_ws, spec) = s3_spec(&s3, "");
    let out = tempfile::tempdir().unwrap();
    let d = cx.new(|_| Downloads::new());
    let first = start(
        cx,
        &d,
        DownloadRequest::new(
            spec.clone(),
            ObjectKey::new(s3.bucket(), "text/hello.txt"),
            out.path().join("a.txt"),
        ),
    );
    wait_real(cx, "the first download", |cx| ended(cx, &d, first));
    assert!(
        !d.update(cx, |d, cx| d.cancel(first, cx)),
        "nothing runs, so cancel does nothing"
    );
    // A press that reaches core just as the first download ends: on a shared session the S3
    // cancel would count it and stop the second download, so the second must still land.
    let handle = d
        .read_with(cx, |d, _| d.cancel_handle(first))
        .expect("the first session's handle");
    handle.cancel();
    let second = start(
        cx,
        &d,
        DownloadRequest::new(
            spec,
            ObjectKey::new(s3.bucket(), "json/doc.json"),
            out.path().join("b.json"),
        ),
    );
    wait_real(cx, "the second download", |cx| ended(cx, &d, second));
    assert_eq!(
        state(cx, &d, second),
        Some(DownloadState::Done { bytes: 26 })
    );
    assert_eq!(
        std::fs::read(out.path().join("b.json")).unwrap(),
        br#"{"name":"bento","tiers":3}"#
    );
}

#[gpui_kit::test]
fn cancel_shows_only_while_a_download_runs_and_stops_a_stalled_connect(cx: &mut TestAppContext) {
    support::init(cx);
    init_tokio(cx);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let (_ws, spec) = stall_spec(listener.local_addr().unwrap().port());
    let out = tempfile::tempdir().unwrap();
    let d = cx.new(|_| Downloads::new());
    let strip = d.clone();
    let (win, _) = cx
        .update(|cx| {
            gpui_kit::open_window(Default::default(), cx, |_, cx| cx.new(|_| Strip(strip)))
        })
        .unwrap();
    let id = start(
        cx,
        &d,
        DownloadRequest::new(spec, ObjectKey::new("b", "k.txt"), out.path().join("k.txt")),
    );
    assert_eq!(state(cx, &d, id), Some(DownloadState::Connecting));
    assert!(
        present(cx, win, ("download-cancel", id)),
        "cancel shows while connecting"
    );
    click_id(cx, win, ("download-cancel", id));
    assert_eq!(state(cx, &d, id), Some(DownloadState::Cancelled));
    assert!(
        !present(cx, win, ("download-cancel", id)),
        "no cancel once it ended"
    );
    assert!(present(cx, win, ("download-dismiss", id)));
    assert!(
        !d.update(cx, |d, cx| d.cancel(id, cx)),
        "a second cancel does nothing"
    );
    assert!(
        files_in(out.path()).is_empty(),
        "no file and no partial file"
    );
    drop(listener);
}

#[gpui_kit::test]
fn a_too_large_object_offers_download_anyway(cx: &mut TestAppContext) {
    support::init(cx);
    init_tokio(cx);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let s3 = rt.block_on(s3_moto());
    let (_ws, spec) = s3_spec(&s3, "");
    let out = tempfile::tempdir().unwrap();
    let dest = out.path().join("big.bin");
    let d = cx.new(|_| Downloads::new());
    let mut req = DownloadRequest::new(
        spec,
        ObjectKey::new(s3.bucket(), "big/three-mib.bin"),
        dest.clone(),
    );
    req.max_size = 1024;
    let id = start(cx, &d, req);
    wait_real(cx, "the refusal", |cx| ended(cx, &d, id));
    assert_eq!(
        state(cx, &d, id),
        Some(DownloadState::TooLarge {
            size: THREE_MIB as u64,
            limit: 1024
        })
    );
    assert!(!dest.exists());
    let again = d
        .update(cx, |d, cx| d.download_anyway(id, cx))
        .expect("a retry at the object's size");
    wait_real(cx, "the retry", |cx| ended(cx, &d, again));
    assert_eq!(
        state(cx, &d, again),
        Some(DownloadState::Done {
            bytes: THREE_MIB as u64
        })
    );
    assert_eq!(
        d.update(cx, |d, cx| d.download_anyway(again, cx)),
        None,
        "only a refusal retries"
    );
}

#[gpui_kit::test]
fn an_existing_file_is_kept_unless_the_user_chose_to_replace_it(cx: &mut TestAppContext) {
    support::init(cx);
    init_tokio(cx);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let s3 = rt.block_on(s3_moto());
    let (_ws, spec) = s3_spec(&s3, "");
    let out = tempfile::tempdir().unwrap();
    let dest = out.path().join("hello.txt");
    std::fs::write(&dest, b"old").unwrap();
    let key = ObjectKey::new(s3.bucket(), "text/hello.txt");
    let d = cx.new(|_| Downloads::new());
    let id = start(
        cx,
        &d,
        DownloadRequest::new(spec.clone(), key.clone(), dest.clone()),
    );
    wait_real(cx, "the refusal", |cx| ended(cx, &d, id));
    assert_eq!(
        state(cx, &d, id),
        Some(DownloadState::Failed(
            "the destination already exists".into()
        ))
    );
    assert_eq!(std::fs::read(&dest).unwrap(), b"old");
    let mut req = DownloadRequest::new(spec, key, dest.clone());
    req.force = replace_existing(&dest);
    assert!(req.force);
    let id = start(cx, &d, req);
    wait_real(cx, "the replace", |cx| ended(cx, &d, id));
    assert_eq!(std::fs::read(&dest).unwrap(), b"hello, bento\n");
}

#[gpui_kit::test]
fn closing_cancels_what_runs_and_leaves_no_partial_file(cx: &mut TestAppContext) {
    support::init(cx);
    init_tokio(cx);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let (_ws, spec) = stall_spec(listener.local_addr().unwrap().port());
    let out = tempfile::tempdir().unwrap();
    let d = cx.new(|_| Downloads::new());
    let id = start(
        cx,
        &d,
        DownloadRequest::new(spec, ObjectKey::new("b", "k.txt"), out.path().join("k.txt")),
    );
    assert_eq!(d.read_with(cx, |d, _| d.running()), ["k.txt"]);
    drop(d.update(cx, |d, cx| d.close_all(cx)));
    wait_real(cx, "the cancel", |cx| ended(cx, &d, id));
    assert_eq!(state(cx, &d, id), Some(DownloadState::Cancelled));
    assert!(d.read_with(cx, |d, _| d.running()).is_empty());
    assert!(files_in(out.path()).is_empty());
    drop(listener);
}

#[test]
fn download_texts_name_sizes_and_reasons() {
    assert_eq!(progress_text(0, None), "0 bytes");
    assert_eq!(
        progress_text(1 << 20, Some(3 << 20)),
        "1.0 MiB of 3.0 MiB (33%)"
    );
    assert_eq!(progress_text(0, Some(0)), "0 bytes of 0 bytes (100%)");
    assert_eq!(
        download_error_text(&DownloadError::Exists),
        "the destination already exists"
    );
    let missing = DownloadError::Remote(ObjectBrowseError::NotFound {
        key: "s3://b/a\u{1b}x".into(),
    });
    assert_eq!(download_error_text(&missing), "not found: s3://b/a\\u{1b}x");
    let big = DownloadError::TooLarge {
        size: 3 << 20,
        limit: 1 << 20,
    };
    assert_eq!(
        download_error_text(&big),
        "the object is 3.0 MiB, over the 1.0 MiB limit"
    );
}

#[test]
fn the_save_folder_is_the_last_one_then_downloads_then_home() {
    let (a, b, c) = (Path::new("/a"), Path::new("/b"), Path::new("/c"));
    assert_eq!(download_dir(Some(a), Some(b), Some(c)), Some(a.to_owned()));
    assert_eq!(download_dir(None, Some(b), Some(c)), Some(b.to_owned()));
    assert_eq!(download_dir(None, None, Some(c)), Some(c.to_owned()));
    assert_eq!(download_dir(None, None, None), None);
}
