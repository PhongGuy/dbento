use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::{resolve_credentials, resolve_region};
use crate::secrets::Secret;
use crate::workspace::Workspace;

fn ws(keys: &str) -> Workspace {
    let text = format!("dbento = 1\n[connections.d]\nengine = \"dynamodb\"\n{keys}");
    Workspace::from_text(PathBuf::from("/ws"), Path::new("/ws/dbento.toml"), &text).unwrap()
}

fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    move |name| map.get(name).cloned()
}

#[test]
fn the_region_comes_from_the_file_then_aws_region_then_aws_default_region() {
    let file = ws("region = \"eu-west-1\"\n");
    let bare = ws("");
    let (file, bare) = (file.declared("d").unwrap(), bare.declared("d").unwrap());
    let both = env(&[
        ("AWS_REGION", "us-east-1"),
        ("AWS_DEFAULT_REGION", "ap-southeast-2"),
    ]);
    assert_eq!(resolve_region(file, &both).as_deref(), Ok("eu-west-1"));
    assert_eq!(resolve_region(bare, &both).as_deref(), Ok("us-east-1"));
    let default_only = env(&[("AWS_REGION", ""), ("AWS_DEFAULT_REGION", "ap-southeast-2")]);
    assert_eq!(
        resolve_region(bare, &default_only).as_deref(),
        Ok("ap-southeast-2")
    );
    let m = resolve_region(bare, env(&[])).unwrap_err();
    assert!(
        m.contains("`region`") && m.contains("AWS_REGION") && m.contains("AWS_DEFAULT_REGION"),
        "{m}"
    );
}

#[test]
fn a_file_key_takes_the_resolved_secret_and_no_session_token() {
    let w = ws("access_key_id = \"AKIDFILE\"\n");
    let c = w.declared("d").unwrap();
    let secret = Secret::new("s3cret");
    let vars = env(&[
        ("AWS_ACCESS_KEY_ID", "AKIDENV"),
        ("AWS_SECRET_ACCESS_KEY", "envsecret"),
        ("AWS_SESSION_TOKEN", "tok"),
    ]);
    let creds = resolve_credentials(c, Some(&secret), &vars).unwrap();
    assert_eq!(
        (
            creds.access_key_id.as_str(),
            creds.secret_access_key.expose(),
            creds.session_token.is_none()
        ),
        ("AKIDFILE", "s3cret", true)
    );
    let m = resolve_credentials(c, None, &vars).unwrap_err();
    assert!(m.contains("secret access key"), "{m}");
}

#[test]
fn without_a_file_key_every_credential_comes_from_the_variables() {
    let w = ws("");
    let c = w.declared("d").unwrap();
    let creds = resolve_credentials(
        c,
        None,
        env(&[
            ("AWS_ACCESS_KEY_ID", "AKIDENV"),
            ("AWS_SECRET_ACCESS_KEY", "envsecret"),
            ("AWS_SESSION_TOKEN", "tok"),
        ]),
    )
    .unwrap();
    assert_eq!(
        (
            creds.access_key_id.as_str(),
            creds.secret_access_key.expose(),
            creds.session_token.as_ref().map(Secret::expose)
        ),
        ("AKIDENV", "envsecret", Some("tok"))
    );
    let no_token = resolve_credentials(
        c,
        None,
        env(&[
            ("AWS_ACCESS_KEY_ID", "A"),
            ("AWS_SECRET_ACCESS_KEY", "S"),
            ("AWS_SESSION_TOKEN", ""),
        ]),
    )
    .unwrap();
    assert!(no_token.session_token.is_none());
    for (pairs, missing) in [
        (vec![("AWS_SECRET_ACCESS_KEY", "S")], "AWS_ACCESS_KEY_ID"),
        (vec![("AWS_ACCESS_KEY_ID", "A")], "AWS_SECRET_ACCESS_KEY"),
        (
            vec![("AWS_ACCESS_KEY_ID", ""), ("AWS_SECRET_ACCESS_KEY", "S")],
            "AWS_ACCESS_KEY_ID",
        ),
    ] {
        let m = resolve_credentials(c, None, env(&pairs)).unwrap_err();
        assert!(m.contains(missing), "{m}");
    }
}

#[test]
fn credentials_never_print_their_secrets() {
    let w = ws("");
    let creds = resolve_credentials(
        w.declared("d").unwrap(),
        None,
        env(&[
            ("AWS_ACCESS_KEY_ID", "AKIDENV"),
            ("AWS_SECRET_ACCESS_KEY", "envsecret"),
            ("AWS_SESSION_TOKEN", "sessiontoken"),
        ]),
    )
    .unwrap();
    let shown = format!("{creds:?}");
    assert!(
        shown.contains("AKIDENV")
            && !shown.contains("envsecret")
            && !shown.contains("sessiontoken"),
        "{shown}"
    );
}
