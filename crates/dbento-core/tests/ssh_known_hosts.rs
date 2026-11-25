//! Dbento's own known_hosts file through the public API; the user's ~/.ssh is only read.
use dbento_core::{
    HostKey, SshHome, StatePaths, forget_host_key, trust_host_key, user_known_hosts_lists,
};

const A: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEpl7XIrO05PhGnrhKwH5QLVhB9hgippRySbUyJFSjRd";
/// `ssh-keygen -lf` of `A`.
const A_FINGERPRINT: &str = "SHA256:SWMXbyoWDG0JTmWoVE6YAu0/i27K8+4WoKsvbXN8I20";

#[test]
fn a_host_key_shows_its_sha256_fingerprint_and_algorithm() {
    let key = HostKey::from_openssh("db.example", 2222, A).unwrap();
    assert_eq!(key.fingerprint(), A_FINGERPRINT);
    assert_eq!(key.algorithm(), "ssh-ed25519");
    assert_eq!((key.host(), key.port()), ("db.example", 2222));
    assert!(HostKey::from_openssh("db.example", 22, "ssh-ed25519 !!!").is_none());
}

#[test]
fn trust_appends_once_and_forget_removes_only_that_identity() {
    let state = tempfile::tempdir().unwrap();
    let paths = StatePaths::in_dir(&state.path().join("nested"));
    let key = HostKey::from_openssh("db.example", 2222, A).unwrap();
    trust_host_key(&paths, &key).unwrap();
    trust_host_key(&paths, &key).unwrap();
    trust_host_key(
        &paths,
        &HostKey::from_openssh("Other.Example", 22, A).unwrap(),
    )
    .unwrap();
    let file = &paths.ssh_known_hosts;
    assert_eq!(
        std::fs::read_to_string(file).unwrap(),
        format!("[db.example]:2222 {A}\nother.example {A}\n")
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    assert_eq!(forget_host_key(&paths, "db.example", 2222).unwrap(), 1);
    assert_eq!(
        std::fs::read_to_string(file).unwrap(),
        format!("other.example {A}\n")
    );
    assert_eq!(forget_host_key(&paths, "db.example", 2222).unwrap(), 0);
}

#[test]
fn forget_on_a_missing_file_removes_nothing_and_creates_nothing() {
    let state = tempfile::tempdir().unwrap();
    let paths = StatePaths::in_dir(state.path());
    assert_eq!(forget_host_key(&paths, "db.example", 22).unwrap(), 0);
    assert!(!paths.ssh_known_hosts.exists());
}

#[test]
fn the_user_known_hosts_is_only_read() {
    let home = tempfile::tempdir().unwrap();
    let ssh = home.path().join(".ssh");
    std::fs::create_dir_all(&ssh).unwrap();
    let text = format!("[db.example]:2222 {A}\n");
    std::fs::write(ssh.join("known_hosts"), &text).unwrap();
    let home_ = SshHome::new(home.path(), "tester");
    assert!(user_known_hosts_lists(&home_, "db.example", 2222));
    assert!(!user_known_hosts_lists(&home_, "db.example", 22));
    assert_eq!(
        std::fs::read_to_string(ssh.join("known_hosts")).unwrap(),
        text
    );
}
