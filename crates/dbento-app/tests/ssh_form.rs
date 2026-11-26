//! The SSH section's values as dbento.toml gets them: untouched keys keep their spelling.
use dbento_app::ssh_form::{
    SSH_HOST_NOTE, SSH_MONGO_HOST_NOTE, SshAuthChoice, SshField, SshTyped, identity_warning,
    shows_passphrase, shows_password, ssh_host_note, ssh_shown, ssh_spec, takes_ssh,
};
use dbento_core::{Engine, SshAuth, SshSpec, ssh_tunnels};
use std::path::Path;

fn typed(host: &str) -> SshTyped {
    SshTyped {
        on: true,
        host: host.into(),
        port: String::new(),
        user: String::new(),
        auth: SshAuthChoice::Default,
        identity_file: String::new(),
        keepalive: true,
    }
}

#[test]
fn unticked_writes_no_table_and_drops_a_loaded_one() {
    let off = SshTyped {
        on: false,
        ..typed("bastion")
    };
    assert_eq!(ssh_spec(None, &off), Ok(None));
    assert_eq!(ssh_spec(Some(&SshSpec::new("jump")), &off), Ok(None));
}

#[test]
fn a_host_alone_writes_only_the_host() {
    assert_eq!(
        ssh_spec(None, &typed(" bastion ")),
        Ok(Some(SshSpec::new("bastion")))
    );
}

#[test]
fn every_field_is_written_as_typed() {
    let t = SshTyped {
        port: "2222".into(),
        user: "ops".into(),
        auth: SshAuthChoice::Key,
        identity_file: "~/.ssh/id_ed25519".into(),
        keepalive: false,
        ..typed("bastion")
    };
    let mut want = SshSpec::new("bastion");
    want.port = Some(2222);
    want.user = Some("ops".into());
    want.auth = Some(SshAuth::Key);
    want.identity_file = Some("~/.ssh/id_ed25519".into());
    want.keepalive = Some(false);
    assert_eq!(ssh_spec(None, &t), Ok(Some(want)));
}

#[test]
fn an_untouched_table_comes_back_unchanged() {
    let mut loaded = SshSpec::new("jump");
    loaded.port = Some(22);
    loaded.auth = Some(SshAuth::Agent);
    loaded.keepalive = Some(true);
    assert_eq!(
        ssh_spec(Some(&loaded), &ssh_shown(Some(&loaded))),
        Ok(Some(loaded.clone()))
    );
}

#[test]
fn keepalive_is_written_only_when_it_differs_from_what_was_there() {
    let fresh = ssh_spec(None, &typed("b")).unwrap().unwrap();
    assert_eq!(fresh.keepalive, None, "the default stays absent");
    let mut loaded = SshSpec::new("b");
    loaded.keepalive = Some(false);
    let back_on = SshTyped {
        keepalive: true,
        ..ssh_shown(Some(&loaded))
    };
    let again = ssh_spec(Some(&loaded), &back_on).unwrap().unwrap();
    assert_eq!(again.keepalive, Some(true), "ticked again");
}

#[test]
fn refusals_name_their_field() {
    assert_eq!(
        ssh_spec(None, &typed("  ")),
        Err((SshField::Host, "Enter the SSH host".to_owned()))
    );
    let port = SshTyped {
        port: "+22".into(),
        ..typed("b")
    };
    assert_eq!(
        ssh_spec(None, &port),
        Err((
            SshField::Port,
            "Port must be a number from 1 to 65535".to_owned()
        ))
    );
    for auth in [SshAuthChoice::Agent, SshAuthChoice::Password] {
        let t = SshTyped {
            auth,
            identity_file: "~/.ssh/id".into(),
            ..typed("b")
        };
        assert_eq!(
            ssh_spec(None, &t),
            Err((
                SshField::Identity,
                "An identity file needs auth Key or Default".to_owned()
            )),
            "{auth:?}"
        );
    }
}

#[test]
fn ssh_is_offered_exactly_where_core_tunnels() {
    for e in [Engine::Postgres, Engine::Mysql, Engine::Mssql] {
        assert!(takes_ssh(e), "{e:?}");
    }
    for e in [Engine::Sqlite, Engine::Duckdb, Engine::Dynamodb, Engine::S3] {
        assert!(!takes_ssh(e), "{e:?}");
    }
    // Feature-gated in core; the app follows whatever this build of core says.
    for e in [Engine::Redis, Engine::Surrealdb, Engine::Mongodb] {
        assert_eq!(takes_ssh(e), ssh_tunnels(e), "{e:?}");
    }
}

#[test]
fn which_secret_rows_each_auth_shows() {
    use SshAuthChoice::{Agent, Default, Key, Password};
    assert_eq!(
        [Default, Agent, Key, Password].map(shows_password),
        [true, false, false, true]
    );
    assert_eq!(
        [Default, Agent, Key, Password].map(shows_passphrase),
        [true, false, true, false]
    );
}

#[test]
fn only_an_absolute_identity_outside_home_warns() {
    let home = Path::new("/home/t");
    assert!(
        identity_warning("/opt/keys/id", Some(home)).is_some(),
        "outside"
    );
    assert!(
        identity_warning("/home/t/.ssh/id", Some(home)).is_none(),
        "inside"
    );
    assert!(identity_warning("~/.ssh/id", Some(home)).is_none(), "tilde");
    assert!(
        identity_warning("keys/id", Some(home)).is_none(),
        "relative"
    );
    assert!(identity_warning("", Some(home)).is_none(), "empty");
    assert!(
        identity_warning("/opt/keys/id", None).is_some(),
        "no home known"
    );
}

#[test]
fn mongodb_gets_its_own_host_note() {
    assert_eq!(ssh_host_note(Engine::Mongodb), SSH_MONGO_HOST_NOTE);
    for e in [
        Engine::Postgres,
        Engine::Mysql,
        Engine::Mssql,
        Engine::Redis,
        Engine::Surrealdb,
    ] {
        assert_eq!(ssh_host_note(e), SSH_HOST_NOTE, "{e:?}");
    }
}
