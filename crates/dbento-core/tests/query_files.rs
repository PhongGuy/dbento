use dbento_core::{QueryFileError, Workspace};
use std::fs;
use std::path::Path;

fn workspace() -> (tempfile::TempDir, Workspace) {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("dbento.toml"),
        "dbento = 1\n[connections.lite]\nengine = \"sqlite\"\npath = \"lite.db\"\n",
    )
    .unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    (dir, ws)
}

#[test]
fn a_written_query_reads_back_and_binds_to_its_folder() {
    let (dir, ws) = workspace();
    let q = ws
        .write_query(Path::new("queries/lite/top.sql"), "SELECT 1;\n")
        .unwrap();
    assert_eq!(
        (q.path(), q.connection()),
        (Path::new("queries/lite/top.sql"), Some("lite"))
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("queries/lite/top.sql")).unwrap(),
        "SELECT 1;\n"
    );
    assert_eq!(ws.saved_queries().unwrap(), std::slice::from_ref(&q));
    assert_eq!(ws.read_query(&q).unwrap(), "SELECT 1;\n");
    ws.write_query(Path::new("queries/lite/top.sql"), "SELECT 2")
        .unwrap();
    assert_eq!(ws.read_query(&q).unwrap(), "SELECT 2");
    let names: Vec<_> = fs::read_dir(dir.path().join("queries/lite"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, ["top.sql"], "no temporary file is left behind");
}

#[test]
fn new_folders_are_created_and_an_unknown_folder_binds_nothing() {
    let (_dir, ws) = workspace();
    let q = ws
        .write_query(Path::new("queries/reports/2026/q.sql"), "SELECT 3")
        .unwrap();
    assert_eq!(q.connection(), None);
    assert_eq!(ws.read_query(&q).unwrap(), "SELECT 3");
}

#[test]
fn paths_outside_queries_are_refused_and_write_nothing() {
    let (dir, ws) = workspace();
    let before = fs::read_to_string(dir.path().join("dbento.toml")).unwrap();
    let absolute = dir.path().join("queries/abs.sql");
    for rel in [
        Path::new("../x.sql"),
        absolute.as_path(),
        Path::new("notes/x.sql"),
        Path::new("queries/../dbento.toml"),
        Path::new("queries/lite/../../x.sql"),
        Path::new("queries"),
    ] {
        let err = ws.write_query(rel, "DROP TABLE t").unwrap_err();
        assert!(
            matches!(err, QueryFileError::OutsideQueries { .. }),
            "{rel:?}: {err:?}"
        );
    }
    assert_eq!(
        fs::read_to_string(dir.path().join("dbento.toml")).unwrap(),
        before
    );
    assert!(!dir.path().join("x.sql").exists() && !absolute.exists());
}

#[cfg(unix)]
#[test]
fn a_symlink_out_of_queries_is_refused() {
    let (dir, ws) = workspace();
    let outside = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("queries")).unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("queries/out")).unwrap();
    for rel in ["queries/out/x.sql", "queries/out/newdir/deeper/x.sql"] {
        let err = ws.write_query(Path::new(rel), "SELECT 1").unwrap_err();
        assert!(
            matches!(err, QueryFileError::OutsideQueries { .. }),
            "{rel}: {err:?}"
        );
    }
    assert_eq!(
        fs::read_dir(outside.path()).unwrap().count(),
        0,
        "nothing created outside"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_new_query_follows_the_umask_and_a_rewrite_keeps_the_mode() {
    use std::os::unix::fs::PermissionsExt as _;
    const NAME: &str = "a_new_query_follows_the_umask_and_a_rewrite_keeps_the_mode";
    // The umask is per process and core forbids unsafe, so the check runs in a child under umask 027.
    if std::env::var_os("DBENTO_UMASK_CHILD").is_none() {
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg("umask 027 && exec \"$0\" --exact \"$1\" --test-threads 1")
            .arg(std::env::current_exe().unwrap())
            .arg(NAME)
            .env("DBENTO_UMASK_CHILD", "1")
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    let (dir, ws) = workspace();
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    let file = dir.path().join("queries/q.sql");
    ws.write_query(Path::new("queries/q.sql"), "SELECT 1")
        .unwrap();
    assert_eq!(mode(&file), 0o640);
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    ws.write_query(Path::new("queries/q.sql"), "SELECT 2")
        .unwrap();
    assert_eq!(mode(&file), 0o600);
}

#[test]
fn reading_reports_bad_text_and_missing_files() {
    let (dir, ws) = workspace();
    fs::create_dir_all(dir.path().join("queries")).unwrap();
    fs::write(dir.path().join("queries/bad.sql"), b"SELECT '\xff'").unwrap();
    let bad = ws.saved_queries().unwrap().remove(0);
    assert!(matches!(
        ws.read_query(&bad),
        Err(QueryFileError::NotUtf8 { .. })
    ));
    fs::remove_file(dir.path().join("queries/bad.sql")).unwrap();
    assert!(matches!(
        ws.read_query(&bad),
        Err(QueryFileError::Io { .. })
    ));
}

// Hardening beyond the brief: a symlinked queries/ itself, dangling links, NUL bytes,
// and reads of files whose size the workspace does not control.

#[cfg(unix)]
#[test]
fn a_symlinked_queries_folder_works_and_stays_contained() {
    let (dir, ws) = workspace();
    let shared = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    fs::write(shared.path().join("x.sql"), "SELECT 1").unwrap();
    std::os::unix::fs::symlink(shared.path(), dir.path().join("queries")).unwrap();
    let listed = ws.saved_queries().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(ws.read_query(&listed[0]).unwrap(), "SELECT 1");
    let q = ws
        .write_query(Path::new("queries/lite/y.sql"), "SELECT 2")
        .unwrap();
    assert_eq!(q.connection(), Some("lite"));
    assert_eq!(
        fs::read_to_string(shared.path().join("lite/y.sql")).unwrap(),
        "SELECT 2"
    );
    // Inside the resolved folder, a link that leaves it is still refused.
    std::os::unix::fs::symlink(elsewhere.path(), shared.path().join("out")).unwrap();
    let err = ws
        .write_query(Path::new("queries/out/z.sql"), "SELECT 3")
        .unwrap_err();
    assert!(
        matches!(err, QueryFileError::OutsideQueries { .. }),
        "{err:?}"
    );
    assert_eq!(fs::read_dir(elsewhere.path()).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn a_query_file_linking_out_of_queries_is_not_listed_read_or_written() {
    let (dir, ws) = workspace();
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("credentials");
    fs::write(&secret, "aws_secret_access_key = hunter2").unwrap();
    fs::create_dir_all(dir.path().join("queries")).unwrap();
    let link = dir.path().join("queries/leak.sql");
    std::os::unix::fs::symlink(&secret, &link).unwrap();
    assert_eq!(
        ws.saved_queries().unwrap(),
        [],
        "an escaping link is not offered"
    );

    // A SavedQuery for the same path, obtained while it was a regular file.
    fs::remove_file(&link).unwrap();
    let q = ws
        .write_query(Path::new("queries/leak.sql"), "SELECT 1")
        .unwrap();
    fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(&secret, &link).unwrap();
    let err = ws.read_query(&q).unwrap_err();
    assert!(
        matches!(err, QueryFileError::OutsideQueries { .. }),
        "{err:?}"
    );

    let err = ws
        .write_query(Path::new("queries/leak.sql"), "SELECT 2")
        .unwrap_err();
    assert!(
        matches!(err, QueryFileError::OutsideQueries { .. }),
        "{err:?}"
    );
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink(),
        "link left as is"
    );
    assert_eq!(
        fs::read_to_string(&secret).unwrap(),
        "aws_secret_access_key = hunter2"
    );
}

#[cfg(unix)]
#[test]
fn a_query_file_linking_inside_queries_is_listed_and_read() {
    let (dir, ws) = workspace();
    fs::create_dir_all(dir.path().join("queries/lite")).unwrap();
    fs::write(dir.path().join("queries/lite/real.sql"), "SELECT 1").unwrap();
    std::os::unix::fs::symlink("lite/real.sql", dir.path().join("queries/alias.sql")).unwrap();
    let listed = ws.saved_queries().unwrap();
    assert_eq!(listed.len(), 2);
    let alias = listed
        .iter()
        .find(|q| q.path().ends_with("alias.sql"))
        .unwrap();
    assert_eq!(ws.read_query(alias).unwrap(), "SELECT 1");
}

#[cfg(unix)]
#[test]
fn a_dangling_symlink_folder_is_refused_and_not_followed() {
    let (dir, ws) = workspace();
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("made-later");
    fs::create_dir_all(dir.path().join("queries")).unwrap();
    std::os::unix::fs::symlink(&target, dir.path().join("queries/dangling")).unwrap();
    let err = ws
        .write_query(Path::new("queries/dangling/x.sql"), "SELECT 1")
        .unwrap_err();
    assert!(
        matches!(err, QueryFileError::OutsideQueries { .. }),
        "{err:?}"
    );
    assert!(
        !target.exists(),
        "no folder created through the dangling link"
    );
}

#[cfg(unix)]
#[test]
fn a_folder_swapped_for_a_symlink_after_listing_is_refused_on_read() {
    let (dir, ws) = workspace();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("x.sql"), "SELECT 'secret'").unwrap();
    let q = ws
        .write_query(Path::new("queries/sub/x.sql"), "SELECT 1")
        .unwrap();
    fs::remove_dir_all(dir.path().join("queries/sub")).unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("queries/sub")).unwrap();
    assert!(matches!(
        ws.read_query(&q),
        Err(QueryFileError::OutsideQueries { .. })
    ));
}

#[test]
fn a_name_with_a_nul_byte_is_an_error_and_writes_nothing() {
    let (dir, ws) = workspace();
    let err = ws
        .write_query(Path::new("queries/a\0b.sql"), "SELECT 1")
        .unwrap_err();
    assert!(
        matches!(
            err,
            QueryFileError::Io { .. } | QueryFileError::OutsideQueries { .. }
        ),
        "{err:?}"
    );
    let left = fs::read_dir(dir.path().join("queries")).map_or(0, |d| d.count());
    assert_eq!(left, 0, "no temporary file is left behind");
}

#[cfg(unix)]
#[test]
fn a_query_that_is_not_a_regular_file_is_refused_without_reading_it() {
    let (dir, ws) = workspace();
    let q = ws
        .write_query(Path::new("queries/q.sql"), "SELECT 1")
        .unwrap();
    let file = dir.path().join("queries/q.sql");
    fs::remove_file(&file).unwrap();
    fs::create_dir(&file).unwrap();
    assert!(matches!(ws.read_query(&q), Err(QueryFileError::Io { .. })));
    // A link to a device leaves queries/: never listed, and refused on read.
    fs::remove_dir(&file).unwrap();
    std::os::unix::fs::symlink("/dev/zero", &file).unwrap();
    assert_eq!(ws.saved_queries().unwrap(), []);
    assert!(matches!(
        ws.read_query(&q),
        Err(QueryFileError::OutsideQueries { .. })
    ));
}

#[test]
fn an_oversized_query_is_refused_without_reading_it_all() {
    let (dir, ws) = workspace();
    fs::create_dir_all(dir.path().join("queries")).unwrap();
    let file = fs::File::create(dir.path().join("queries/huge.sql")).unwrap();
    file.set_len(dbento_core::MAX_QUERY_FILE_BYTES + 1).unwrap();
    let q = ws.saved_queries().unwrap().remove(0);
    assert!(matches!(ws.read_query(&q), Err(QueryFileError::Io { .. })));
}

#[test]
fn reading_after_the_queries_folder_is_gone_is_a_missing_file() {
    let (dir, ws) = workspace();
    let q = ws
        .write_query(Path::new("queries/sub/x.sql"), "SELECT 1")
        .unwrap();
    fs::remove_dir_all(dir.path().join("queries/sub")).unwrap();
    assert!(matches!(ws.read_query(&q), Err(QueryFileError::Io { .. })));
    fs::remove_dir_all(dir.path().join("queries")).unwrap();
    assert!(matches!(ws.read_query(&q), Err(QueryFileError::Io { .. })));
}

#[test]
fn query_exists_sees_the_target_itself_through_inner_links() {
    let (dir, ws) = workspace();
    assert!(
        !ws.query_exists(Path::new("queries/lite/a.sql")).unwrap(),
        "no queries/ yet"
    );
    ws.write_query(Path::new("queries/real/top.sql"), "SELECT 0")
        .unwrap();
    std::os::unix::fs::symlink(
        dir.path().join("queries/real"),
        dir.path().join("queries/lite"),
    )
    .unwrap();
    assert!(ws.query_exists(Path::new("queries/real/top.sql")).unwrap());
    assert!(!ws.query_exists(Path::new("queries/real/new.sql")).unwrap());
    // The listing does not walk the linked folder, but the target exists all the same.
    let listed = ws.saved_queries().unwrap();
    assert!(
        listed
            .iter()
            .all(|q| q.path() != Path::new("queries/lite/top.sql"))
    );
    assert!(ws.query_exists(Path::new("queries/lite/top.sql")).unwrap());
}

#[test]
fn query_exists_refuses_what_read_and_write_refuse() {
    let (dir, ws) = workspace();
    fs::create_dir_all(dir.path().join("queries")).unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("x.sql"), "SELECT 1").unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("queries/out")).unwrap();
    for rel in [
        "queries/../x.sql",
        "/etc/x.sql",
        "queries",
        "queries/out/x.sql",
    ] {
        assert!(
            matches!(
                ws.query_exists(Path::new(rel)),
                Err(QueryFileError::OutsideQueries { .. })
            ),
            "{rel}"
        );
    }
}

#[test]
fn read_query_at_reads_a_path_and_says_none_when_nothing_is_there() {
    let (dir, ws) = workspace();
    let rel = Path::new("queries/lite/a.sql");
    assert_eq!(ws.read_query_at(rel).unwrap(), None, "no queries/ yet");
    ws.write_query(rel, "SELECT 1").unwrap();
    assert_eq!(ws.read_query_at(rel).unwrap().as_deref(), Some("SELECT 1"));
    fs::remove_file(dir.path().join(rel)).unwrap();
    assert_eq!(ws.read_query_at(rel).unwrap(), None);
    fs::create_dir_all(dir.path().join(rel)).unwrap();
    assert!(ws.read_query_at(rel).is_err(), "a folder is not a query");
    assert!(ws.read_query_at(Path::new("../x.sql")).is_err());
}

#[test]
fn query_io_errors_name_the_file_relative_to_the_workspace() {
    let (dir, ws) = workspace();
    let q = ws
        .write_query(Path::new("queries/q.sql"), "SELECT 1")
        .unwrap();
    let file = dir.path().join("queries/q.sql");
    fs::remove_file(&file).unwrap();
    fs::create_dir(&file).unwrap();
    let read = ws.read_query(&q).unwrap_err().to_string();
    assert_eq!(read, "cannot access queries/q.sql: not a regular file");
    // A file where a folder must go fails the write's folder creation.
    fs::write(dir.path().join("queries/plain"), "").unwrap();
    let write = ws
        .write_query(Path::new("queries/plain/x.sql"), "SELECT 1")
        .unwrap_err()
        .to_string();
    assert!(write.starts_with("cannot access queries/plain"), "{write}");
    fs::remove_dir_all(dir.path().join("queries")).unwrap();
    let gone = ws.read_query(&q).unwrap_err().to_string();
    assert!(gone.starts_with("cannot access queries/q.sql: "), "{gone}");
}
