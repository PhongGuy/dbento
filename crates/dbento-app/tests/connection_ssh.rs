//! The connection form's SSH section: shown for tunnelled engines once ticked, with its
//! notes, and written to dbento.toml as an `ssh` table under the entry.
use crate::support::{self, click_id, fill, form, label_of, opened, present, wait_real};
use dbento_app::connection_dialog::Field;
use dbento_app::ssh_form::{SSH_HOST_NOTE, SSH_IDENTITY_WARNING, SSH_PLAINTEXT_NOTE};
use dbento_core::{Secret, SecretKey, SecretStore};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, ScrollDelta, TestAppContext, point, px};
use std::sync::Arc;

const TUN: &str = "\n[connections.tun]\nengine = \"postgres\"\nhost = \"db\"\ndatabase = \"d\"\nuser = \"u\"\n[connections.tun.ssh]\nhost = \"jump\"\nuser = \"ops\"\n";

fn new_pg(cx: &mut TestAppContext, o: &support::Opened) {
    click_id(cx, o.win, "connection-new");
    form(cx, o);
    fill(cx, o, Field::Name, "pg");
    fill(cx, o, Field::Host, "db.internal");
    fill(cx, o, Field::Database, "d");
    fill(cx, o, Field::User, "u");
}

fn segment(cx: &mut TestAppContext, o: &support::Opened, id: &'static str, ix: usize) {
    cx.update_window(o.win, |_, w, cx| w.within(id).click(ix, cx))
        .unwrap();
    cx.run_until_parked();
}

fn toml(o: &support::Opened) -> String {
    std::fs::read_to_string(o.fx.dir.path().join("dbento.toml")).unwrap()
}

#[gpui_kit::test]
fn the_ssh_rows_show_once_ticked_with_the_host_note(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    new_pg(cx, &o);
    assert!(
        present(cx, o.win, "conn-ssh"),
        "the tick shows for Postgres"
    );
    assert!(
        !present(cx, o.win, "conn-ssh-host"),
        "rows wait for the tick"
    );
    click_id(cx, o.win, "conn-ssh");
    for id in [
        "conn-ssh-host",
        "conn-ssh-port",
        "conn-ssh-user",
        "conn-ssh-auth",
        "conn-ssh-identity",
        "conn-ssh-password",
        "conn-ssh-key-passphrase",
        "conn-ssh-keepalive",
        "conn-ssh-note",
    ] {
        assert!(present(cx, o.win, id), "{id}");
    }
    assert_eq!(
        label_of(cx, o.win, "conn-ssh-note").as_deref(),
        Some(SSH_HOST_NOTE)
    );
    segment(cx, &o, "conn-ssh-auth", 1);
    assert!(
        !present(cx, o.win, "conn-ssh-password"),
        "agent asks for no password"
    );
    assert!(
        !present(cx, o.win, "conn-ssh-identity"),
        "nor an identity file"
    );
}

#[gpui_kit::test]
fn sqlite_has_no_ssh_section(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    click_id(cx, o.win, "connection-new");
    form(cx, &o);
    segment(cx, &o, "conn-engine", 1);
    assert!(!present(cx, o.win, "conn-ssh"), "no tick for a file engine");
}

#[gpui_kit::test]
fn tls_off_and_an_absolute_identity_add_their_notes(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    new_pg(cx, &o);
    click_id(cx, o.win, "conn-ssh");
    assert!(!present(cx, o.win, "conn-ssh-plaintext"), "TLS not off yet");
    // The SSH rows push TLS below the fold, so scroll to it as a user would.
    cx.update_window(o.win, |_, w, cx| {
        w.scroll(
            "conn-body",
            ScrollDelta::Pixels(point(px(0.), px(-400.))),
            cx,
        )
    })
    .unwrap();
    cx.run_until_parked();
    segment(cx, &o, "conn-tls", 2);
    assert_eq!(
        label_of(cx, o.win, "conn-ssh-plaintext").as_deref(),
        Some(SSH_PLAINTEXT_NOTE)
    );
    fill(cx, &o, Field::SshIdentity, "/opt/keys/id");
    assert_eq!(
        label_of(cx, o.win, "conn-ssh-identity-warning").as_deref(),
        Some(SSH_IDENTITY_WARNING)
    );
    fill(cx, &o, Field::SshIdentity, "~/.ssh/id_ed25519");
    assert!(
        !present(cx, o.win, "conn-ssh-identity-warning"),
        "a home path is portable"
    );
}

#[gpui_kit::test]
fn saving_writes_the_ssh_table_under_the_entry(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    new_pg(cx, &o);
    click_id(cx, o.win, "conn-ssh");
    fill(cx, &o, Field::SshHost, "bastion");
    fill(cx, &o, Field::SshPort, "2222");
    fill(cx, &o, Field::SshUser, "ops");
    segment(cx, &o, "conn-ssh-auth", 2);
    fill(cx, &o, Field::SshIdentity, "~/.ssh/id_ed25519");
    click_id(cx, o.win, "conn-ssh-keepalive");
    click_id(cx, o.win, "conn-save");
    wait_real(cx, "the dialog closes", |cx| {
        !present(cx, o.win, "conn-dialog")
    });
    let text = toml(&o);
    assert!(
        text.contains("[connections.pg.ssh]\nhost = \"bastion\"\nport = 2222\nuser = \"ops\"\nauth = \"key\"\nidentity_file = \"~/.ssh/id_ed25519\"\nkeepalive = false\n"),
        "{text}"
    );
}

#[gpui_kit::test]
fn a_missing_ssh_host_is_refused_under_its_field(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    new_pg(cx, &o);
    click_id(cx, o.win, "conn-ssh");
    click_id(cx, o.win, "conn-save");
    assert_eq!(
        label_of(cx, o.win, "conn-error").as_deref(),
        Some("Enter the SSH host")
    );
    let at = form(cx, &o).read_with(cx, |f, _| f.error_field());
    assert_eq!(at, Some(Field::SshHost), "under the SSH host");
    assert!(!toml(&o).contains("[connections.pg]"), "nothing written");
}

#[gpui_kit::test]
fn an_existing_table_shows_and_unticking_removes_it(cx: &mut TestAppContext) {
    let o = opened(cx, "", TUN, 0);
    support::gear(cx, o.win, "tun");
    let f = form(cx, &o);
    let host = f.read_with(cx, |f, cx| {
        f.input(Field::SshHost).read(cx).value().to_string()
    });
    assert_eq!(host, "jump", "the loaded host");
    click_id(cx, o.win, "conn-ssh");
    click_id(cx, o.win, "conn-save");
    wait_real(cx, "the dialog closes", |cx| {
        !present(cx, o.win, "conn-dialog")
    });
    let text = toml(&o);
    assert!(
        text.contains("[connections.tun]") && !text.contains("[connections.tun.ssh]"),
        "{text}"
    );
}

fn store_and_dir(
    cx: &mut TestAppContext,
    o: &support::Opened,
) -> (Arc<dyn SecretStore>, std::path::PathBuf) {
    o.shell.read_with(cx, |s, _| {
        (
            Arc::clone(&s.services().store),
            s.workspace().unwrap().dir().to_path_buf(),
        )
    })
}

#[gpui_kit::test]
fn a_typed_ssh_password_goes_to_its_pinned_entry_only(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    new_pg(cx, &o);
    click_id(cx, o.win, "conn-ssh");
    fill(cx, &o, Field::SshHost, "bastion");
    fill(cx, &o, Field::SshUser, "ops");
    segment(cx, &o, "conn-ssh-auth", 3);
    fill(cx, &o, Field::SshPassword, "x");
    // The fixture already holds "x" as other connections' values; saving must add none.
    let before = toml(&o).matches("\"x\"").count();
    click_id(cx, o.win, "conn-save");
    let (st, dir) = store_and_dir(cx, &o);
    let k = SecretKey::ssh_password(&dir, "pg", "ops@bastion:22");
    wait_real(cx, "the stored SSH password", |_| {
        st.get(&k).unwrap().is_some()
    });
    assert_eq!(st.get(&k).unwrap().unwrap().expose(), "x");
    let text = toml(&o);
    assert!(
        text.contains(
            "[connections.pg.ssh]\nhost = \"bastion\"\nuser = \"ops\"\nauth = \"password\"\n"
        ),
        "{text}"
    );
    assert_eq!(
        text.matches("\"x\"").count(),
        before,
        "never in the file: {text}"
    );
}

#[gpui_kit::test]
fn renaming_in_the_form_carries_both_ssh_entries(cx: &mut TestAppContext) {
    let o = opened(cx, "", TUN, 0);
    let (st, dir) = store_and_dir(cx, &o);
    let (pw, pp) = (
        |d: &std::path::Path, n: &str| SecretKey::ssh_password(d, n, "ops@jump:22"),
        |d: &std::path::Path, n: &str| SecretKey::ssh_key_passphrase(d, n),
    );
    st.set(&pw(&dir, "tun"), &Secret::new("x")).unwrap();
    st.set(&pp(&dir, "tun"), &Secret::new("x")).unwrap();
    support::gear(cx, o.win, "tun");
    form(cx, &o);
    fill(cx, &o, Field::Name, "tun2");
    click_id(cx, o.win, "conn-save");
    wait_real(cx, "the moved entries", |_| {
        st.get(&pw(&dir, "tun2")).unwrap().is_some() && st.get(&pp(&dir, "tun2")).unwrap().is_some()
    });
    assert!(
        st.get(&pw(&dir, "tun")).unwrap().is_none(),
        "old password entry gone"
    );
    assert!(
        st.get(&pp(&dir, "tun")).unwrap().is_none(),
        "old passphrase entry gone"
    );
}
