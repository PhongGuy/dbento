use super::{S3Command, parse};
use crate::s3::ObjectKey;

fn words(w: &[&str]) -> Vec<String> {
    w.iter().map(|s| s.to_string()).collect()
}

#[test]
fn the_four_command_shapes_parse() {
    assert!(matches!(parse(&words(&["ls"])), Ok(S3Command::Buckets)));
    match parse(&words(&["ls", "s3://b/p/"])).unwrap() {
        S3Command::List { url, recursive } => {
            assert_eq!((url, recursive), (ObjectKey::new("b", "p/"), false))
        }
        _ => panic!(),
    }
    assert!(matches!(
        parse(&words(&["ls", "-r", "s3://b"])),
        Ok(S3Command::List {
            recursive: true,
            ..
        })
    ));
    assert!(
        matches!(parse(&words(&["stat", "s3://b/k"])), Ok(S3Command::Stat(k)) if k == ObjectKey::new("b", "k"))
    );
    assert!(matches!(
        parse(&words(&["cat", "s3://b/k"])),
        Ok(S3Command::Cat(_))
    ));
}

#[test]
fn bad_shapes_name_the_usage() {
    for bad in [
        &["rm", "s3://b/k"][..],
        &["ls", "b/k"],
        &["stat", "s3://b/"],
        &["cat"],
        &["ls", "-x", "s3://b"],
        &["ls", "s3://b", "extra"],
    ] {
        let e = parse(&words(bad)).unwrap_err();
        assert!(
            e.contains("ls") && e.contains("stat") && e.contains("cat"),
            "{bad:?}: {e}"
        );
    }
}
