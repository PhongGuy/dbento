//! Writing a connection's `[connections.<name>.ssh]` table through `plan_edit`.
use crate::edit::ws;
use dbento_core::{
    ConnectionSpec, Edit, EditError, Engine, SshAuth, SshSpec, Workspace, plan_edit,
};

const BASE: &str = "dbento = 1\n[connections.a]\nengine = \"postgres\"\nhost = \"db\"\ndatabase = \"d\"\nuser = \"u\"\n\n[connections.b]\nengine = \"sqlite\"\npath = \"b.db\"\n";
const WITH_SSH: &str = "dbento = 1\n[connections.a]\nengine = \"postgres\"\nhost = \"db\"\ndatabase = \"d\"\nuser = \"u\"\n[connections.a.ssh]\nhost = \"bastion\"\nport = 22 # jump\n\n[connections.b]\nengine = \"sqlite\"\npath = \"b.db\"\n";

fn put(
    w: &Workspace,
    previous: Option<&str>,
    name: &str,
    spec: ConnectionSpec,
) -> Result<String, EditError> {
    plan_edit(
        w,
        &Edit::PutConnection {
            previous: previous.map(Into::into),
            name: name.into(),
            spec,
        },
    )
    .map(|p| p.new_text().to_owned())
}

#[test]
fn connection_spec_reads_the_ssh_table() {
    let (_d, w) = ws(WITH_SSH);
    let mut want = SshSpec::new("bastion");
    want.port = Some(22);
    assert_eq!(w.connection_spec("a").unwrap().ssh, Some(want));
    assert_eq!(w.connection_spec("b").unwrap().ssh, None);
}

#[test]
fn adding_ssh_puts_its_table_right_under_the_entry() {
    let (_d, w) = ws(BASE);
    let mut s = w.connection_spec("a").unwrap();
    let mut ssh = SshSpec::new("bastion");
    ssh.port = Some(2222);
    s.ssh = Some(ssh);
    assert_eq!(
        put(&w, Some("a"), "a", s).unwrap(),
        BASE.replace(
            "user = \"u\"\n",
            "user = \"u\"\n[connections.a.ssh]\nhost = \"bastion\"\nport = 2222\n"
        )
    );
}

#[test]
fn a_new_connection_writes_its_ssh_table_after_its_keys() {
    let (_d, w) = ws(BASE);
    let mut s = ConnectionSpec::new(Engine::Mysql);
    s.host = Some("db".into());
    s.user = Some("u".into());
    let mut ssh = SshSpec::new("bastion");
    ssh.auth = Some(SshAuth::Agent);
    ssh.keepalive = Some(false);
    s.ssh = Some(ssh);
    assert_eq!(
        put(&w, None, "c", s).unwrap(),
        format!(
            "{BASE}\n[connections.c]\nengine = \"mysql\"\nhost = \"db\"\nuser = \"u\"\n[connections.c.ssh]\nhost = \"bastion\"\nauth = \"agent\"\nkeepalive = false\n"
        )
    );
}

#[test]
fn a_changed_ssh_value_keeps_its_comment() {
    let (_d, w) = ws(WITH_SSH);
    let mut s = w.connection_spec("a").unwrap();
    s.ssh.as_mut().unwrap().port = Some(2200);
    assert_eq!(
        put(&w, Some("a"), "a", s).unwrap(),
        WITH_SSH.replace("port = 22 # jump", "port = 2200 # jump")
    );
}

#[test]
fn clearing_ssh_removes_its_table() {
    let (_d, w) = ws(WITH_SSH);
    let mut s = w.connection_spec("a").unwrap();
    s.ssh = None;
    assert_eq!(put(&w, Some("a"), "a", s).unwrap(), BASE);
}

#[test]
fn a_rename_carries_the_ssh_table() {
    let (_d, w) = ws(WITH_SSH);
    let s = w.connection_spec("a").unwrap();
    assert_eq!(
        put(&w, Some("a"), "z", s).unwrap(),
        WITH_SSH
            .replace("[connections.a]", "[connections.z]")
            .replace("[connections.a.ssh]", "[connections.z.ssh]")
    );
}

#[test]
fn an_inline_ssh_table_is_refused_only_when_the_edit_changes_it() {
    let text = BASE.replace(
        "user = \"u\"\n",
        "user = \"u\"\nssh = { host = \"bastion\" }\n",
    );
    let (_d, w) = ws(&text);
    let mut s = w.connection_spec("a").unwrap();
    s.read_only = Some(true);
    assert!(put(&w, Some("a"), "a", s.clone()).is_ok());
    s.ssh.as_mut().unwrap().port = Some(2200);
    assert_eq!(
        put(&w, Some("a"), "a", s),
        Err(EditError::Unmergeable {
            entry: "a.ssh".into()
        })
    );
}
