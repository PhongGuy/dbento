//! Project query files and consoles collected by `read_datagrip`.
use dbento_core::import::datagrip::{DataGripOptions, read_datagrip};
use dbento_core::import::{ImportSet, ItemKind, SkipReason, WarningKind};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/datagrip");
const ORDERS: &str = "11111111-1111-4111-8111-111111111111";
const ONE_SQLITE: &str = r#"<project version="4"><component name="DataSourceManagerImpl"><data-source source="LOCAL" name="lite" uuid="bbbbbbbb-0000-4000-8000-000000000001"><driver-ref>sqlite.xerial</driver-ref><jdbc-url>jdbc:sqlite:$PROJECT_DIR$/lite.db</jdbc-url></data-source></component></project>"#;

type Row = (Option<String>, String, String);

fn read(consoles: bool) -> ImportSet {
    let root = Path::new(FIXTURES);
    // Newest first, as the CLI's folder scan orders them.
    let products = ["DataGrip2024.2", "DataGrip2024.1", "PyCharm2024.1"];
    let dirs = if consoles {
        products
            .iter()
            .map(|p| root.join("jetbrains").join(p))
            .collect()
    } else {
        Vec::new()
    };
    let opts = DataGripOptions::new()
        .config_dirs(dirs)
        .home(Some(PathBuf::from("/home/fixture")));
    read_datagrip(&root.join("modern"), &opts).unwrap()
}

/// (bound source id, relative path, origin below the fixtures folder) per query.
fn rows(set: &ImportSet) -> Vec<Row> {
    set.queries()
        .iter()
        .map(|q| {
            (
                q.connection().map(str::to_owned),
                q.relative().to_string_lossy().into_owned(),
                q.origin()
                    .strip_prefix(FIXTURES)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            )
        })
        .collect()
}

fn row(conn: Option<&str>, relative: &str, origin: &str) -> Row {
    (
        conn.map(str::to_owned),
        relative.to_owned(),
        origin.to_owned(),
    )
}

fn tree(root: &Path, files: &[(&str, &[u8])]) {
    for (rel, bytes) in files {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
}

#[test]
fn project_query_files_bind_through_the_mapping() {
    let set = read(false);
    assert_eq!(
        rows(&set),
        [
            row(None, "notes.sql", "modern/queries/notes.sql"),
            row(
                Some(ORDERS),
                "orders/top.sql",
                "modern/queries/orders/top.sql"
            ),
            row(None, "Query.sql", "modern/.idea/queries/Query.sql"),
        ]
    );
    assert_eq!(
        set.queries()[1].content(),
        b"select order_id from orders order by total desc limit 10;\n"
    );
}

#[test]
fn consoles_are_deduplicated_and_a_conflict_is_named_after_its_folder() {
    let set = read(true);
    let consoles: Vec<Row> = rows(&set).into_iter().skip(3).collect();
    assert_eq!(
        consoles,
        [
            row(
                Some(ORDERS),
                "console.sql",
                &format!("jetbrains/DataGrip2024.2/consoles/db/{ORDERS}/console.sql")
            ),
            row(
                Some(ORDERS),
                "console-PyCharm2024.1.sql",
                &format!("jetbrains/PyCharm2024.1/consoles/db/{ORDERS}/console.sql")
            ),
        ]
    );
    let skipped: Vec<(String, SkipReason)> = set
        .skipped()
        .iter()
        .filter(|s| s.kind() == ItemKind::Query)
        .map(|s| (s.subject().to_owned(), s.reason().clone()))
        .collect();
    let tunnel = Path::new(FIXTURES).join(
        "jetbrains/DataGrip2024.2/consoles/db/55555555-5555-4555-8555-555555555555/console.sql",
    );
    assert_eq!(
        skipped,
        [(tunnel.display().to_string(), SkipReason::SourceSkipped)]
    );
}

#[cfg(unix)]
#[test]
fn symlinked_query_files_folders_and_consoles_are_skipped_not_followed() {
    use std::os::unix::fs::symlink;
    let tmp = TempDir::new().unwrap();
    let (proj, outside) = (tmp.path().join("p"), tmp.path().join("outside"));
    let config = tmp.path().join("cfg/DataGrip2024.2");
    tree(
        &proj,
        &[
            (".idea/dataSources.xml", ONE_SQLITE.as_bytes()),
            ("queries/real.sql", b"select 1;\n"),
        ],
    );
    tree(&outside, &[("x.sql", b"select 2;\n")]);
    let console_dir = config.join("consoles/db/bbbbbbbb-0000-4000-8000-000000000001");
    std::fs::create_dir_all(&console_dir).unwrap();
    symlink(proj.join("queries/real.sql"), proj.join("queries/link.sql")).unwrap();
    symlink(&outside, proj.join("queries/dir")).unwrap();
    symlink(outside.join("x.sql"), console_dir.join("console.sql")).unwrap();
    let set = read_datagrip(&proj, &DataGripOptions::new().config_dirs(vec![config])).unwrap();
    let relatives: Vec<PathBuf> = set
        .queries()
        .iter()
        .map(|q| q.relative().to_path_buf())
        .collect();
    assert_eq!(relatives, [PathBuf::from("real.sql")]);
    let mut skipped: Vec<(String, SkipReason)> = set
        .skipped()
        .iter()
        .filter(|s| s.kind() == ItemKind::Query)
        .map(|s| (s.subject().to_owned(), s.reason().clone()))
        .collect();
    skipped.sort_by(|a, b| a.0.cmp(&b.0));
    let want: Vec<(String, SkipReason)> = [
        console_dir.join("console.sql"),
        proj.join("queries/dir"),
        proj.join("queries/link.sql"),
    ]
    .iter()
    .map(|p| (p.display().to_string(), SkipReason::Symlink))
    .collect();
    assert_eq!(skipped, want);
}

#[test]
fn a_dotdot_mapping_is_outside_root_and_a_non_utf8_query_is_copied_with_a_warning() {
    let tmp = TempDir::new().unwrap();
    let proj = tmp.path().join("p");
    let mapping = r#"<project version="4"><component name="DataSourcePerFileMappings"><file url="file://$PROJECT_DIR$/queries/../../escape.sql" value="bbbbbbbb-0000-4000-8000-000000000001" /></component></project>"#;
    tree(
        &proj,
        &[
            (".idea/dataSources.xml", ONE_SQLITE.as_bytes()),
            (".idea/data_source_mapping.xml", mapping.as_bytes()),
            ("queries/latin1.sql", b"select '\xe9';\n"),
        ],
    );
    let set = read_datagrip(&proj, &DataGripOptions::new()).unwrap();
    assert_eq!(
        set.skipped()
            .iter()
            .filter(|s| s.reason() == &SkipReason::OutsideRoot)
            .count(),
        1
    );
    assert_eq!(set.queries()[0].content(), b"select '\xe9';\n");
    let latin = proj.join("queries/latin1.sql").display().to_string();
    assert!(
        set.warnings()
            .iter()
            .any(|w| w.subject() == latin && w.kind() == &WarningKind::NotUtf8)
    );
}

#[test]
fn a_dotfiles_copy_scans_its_one_query_folder_once() {
    let tmp = TempDir::new().unwrap();
    tree(
        tmp.path(),
        &[
            ("dataSources.xml", ONE_SQLITE.as_bytes()),
            ("queries/q.sql", b"select 1;\n"),
        ],
    );
    assert_eq!(
        read_datagrip(tmp.path(), &DataGripOptions::new())
            .unwrap()
            .queries()
            .len(),
        1
    );
}

const TWO_SAME_UUID: &str = r#"<project version="4"><component name="DataSourceManagerImpl"><data-source source="LOCAL" name="a" uuid="dddddddd-0000-4000-8000-000000000001"><driver-ref>sqlite.xerial</driver-ref><jdbc-url>jdbc:sqlite:$PROJECT_DIR$/a.db</jdbc-url></data-source><data-source source="LOCAL" name="b" uuid="dddddddd-0000-4000-8000-000000000001"><driver-ref>sqlite.xerial</driver-ref><jdbc-url>jdbc:sqlite:$PROJECT_DIR$/b.db</jdbc-url></data-source></component></project>"#;

#[test]
fn a_duplicated_or_blank_uuid_binds_no_mapping_and_no_console() {
    let tmp = TempDir::new().unwrap();
    let proj = tmp.path().join("p");
    let config = tmp.path().join("cfg/DataGrip2024.2");
    let mapping = r#"<project version="4"><component name="DataSourcePerFileMappings"><file url="file://$PROJECT_DIR$/queries/q.sql" value="dddddddd-0000-4000-8000-000000000001" /><file url="file://$PROJECT_DIR$/queries/r.sql" value="" /></component></project>"#;
    tree(
        &proj,
        &[
            (".idea/dataSources.xml", TWO_SAME_UUID.as_bytes()),
            (".idea/data_source_mapping.xml", mapping.as_bytes()),
            ("queries/q.sql", b"select 1;\n"),
            ("queries/r.sql", b"select 2;\n"),
        ],
    );
    tree(
        &config,
        &[(
            "consoles/db/dddddddd-0000-4000-8000-000000000001/console.sql",
            b"select 3;\n",
        )],
    );
    let set = read_datagrip(&proj, &DataGripOptions::new().config_dirs(vec![config])).unwrap();
    assert_eq!(set.connections().len(), 2);
    assert_eq!(set.queries().len(), 2);
    assert!(set.queries().iter().all(|q| q.connection().is_none()));
}

#[test]
fn only_a_dotdot_mapping_that_would_bind_warns_and_only_once() {
    let tmp = TempDir::new().unwrap();
    let proj = tmp.path().join("p");
    let escape = "file://$PROJECT_DIR$/queries/../../escape.sql";
    let mapping = format!(
        r#"<project version="4"><component name="DataSourcePerFileMappings"><file url="{escape}" value="bbbbbbbb-0000-4000-8000-000000000001" /><file url="{escape}" value="bbbbbbbb-0000-4000-8000-000000000001" /><file url="file://$PROJECT_DIR$/../blank.sql" value="" /><file url="file://$PROJECT_DIR$/../unknown.sql" value="eeeeeeee-0000-4000-8000-000000000001" /></component></project>"#
    );
    tree(
        &proj,
        &[
            (".idea/dataSources.xml", ONE_SQLITE.as_bytes()),
            (".idea/data_source_mapping.xml", mapping.as_bytes()),
        ],
    );
    let set = read_datagrip(&proj, &DataGripOptions::new()).unwrap();
    let outside: Vec<&str> = set
        .skipped()
        .iter()
        .filter(|s| s.reason() == &SkipReason::OutsideRoot)
        .map(|s| s.subject())
        .collect();
    assert_eq!(outside.len(), 1, "{outside:?}");
    assert!(outside[0].ends_with("escape.sql"), "{outside:?}");
}

#[test]
fn a_console_name_taken_twice_gets_a_numbered_folder_suffix() {
    let tmp = TempDir::new().unwrap();
    let proj = tmp.path().join("p");
    let (newer, older) = (
        tmp.path().join("cfg/DataGrip2024.2"),
        tmp.path().join("cfg/DataGrip2024.1"),
    );
    let console = "consoles/db/bbbbbbbb-0000-4000-8000-000000000001";
    tree(&proj, &[(".idea/dataSources.xml", ONE_SQLITE.as_bytes())]);
    tree(
        &newer,
        &[
            (&format!("{console}/console.sql"), b"select 1;\n"),
            (
                &format!("{console}/console-DataGrip2024.1.sql"),
                b"select 2;\n",
            ),
        ],
    );
    tree(
        &older,
        &[(&format!("{console}/console.sql"), b"select 3;\n")],
    );
    let opts = DataGripOptions::new().config_dirs(vec![newer, older]);
    let set = read_datagrip(&proj, &opts).unwrap();
    let names: Vec<String> = set
        .queries()
        .iter()
        .map(|q| q.relative().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        [
            "console-DataGrip2024.1.sql",
            "console.sql",
            "console-DataGrip2024.1-2.sql"
        ]
    );
}

#[cfg(unix)]
#[test]
fn an_unreadable_query_file_aborts_the_read_with_io() {
    use dbento_core::import::datagrip::DataGripError;
    use std::os::unix::fs::PermissionsExt;
    let tmp = TempDir::new().unwrap();
    let proj = tmp.path().join("p");
    tree(
        &proj,
        &[
            (".idea/dataSources.xml", ONE_SQLITE.as_bytes()),
            ("queries/locked.sql", b"select 1;\n"),
        ],
    );
    let locked = proj.join("queries/locked.sql");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    // Root reads through mode 000, so there is nothing to observe there.
    if std::fs::read(&locked).is_ok() {
        return;
    }
    match read_datagrip(&proj, &DataGripOptions::new()) {
        Err(DataGripError::Io { path, .. }) => assert_eq!(path, locked),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_uuid_that_is_not_one_plain_name_scans_no_console_folder() {
    let tmp = TempDir::new().unwrap();
    let proj = tmp.path().join("p");
    let config = tmp.path().join("cfg/DataGrip2024.2");
    let outside = tmp.path().join("outside");
    let uuids = [
        "../../evil/sub".to_owned(),
        "../../../../outside".to_owned(),
        outside.join("abs").to_string_lossy().into_owned(),
        "a/b".to_owned(),
    ];
    let sources: String = uuids
        .iter()
        .enumerate()
        .map(|(i, u)| format!(r#"<data-source source="LOCAL" name="s{i}" uuid="{u}"><driver-ref>sqlite.xerial</driver-ref><jdbc-url>jdbc:sqlite:$PROJECT_DIR$/s{i}.db</jdbc-url></data-source>"#))
        .collect();
    let xml = format!(
        r#"<project version="4"><component name="DataSourceManagerImpl">{sources}</component></project>"#
    );
    tree(&proj, &[(".idea/dataSources.xml", xml.as_bytes())]);
    // Every place a joined uuid could reach holds a query that must not be copied.
    tree(
        &config,
        &[
            ("evil/sub/stolen.sql", b"select 1;\n"),
            ("consoles/db/a/b/stolen.sql", b"select 2;\n"),
        ],
    );
    tree(
        &outside,
        &[
            ("stolen.sql", b"select 3;\n"),
            ("abs/stolen.sql", b"select 4;\n"),
        ],
    );
    let set = read_datagrip(&proj, &DataGripOptions::new().config_dirs(vec![config])).unwrap();
    assert_eq!(set.connections().len(), 4);
    let origins: Vec<&Path> = set.queries().iter().map(|q| q.origin()).collect();
    assert_eq!(origins, Vec::<&Path>::new());
}
