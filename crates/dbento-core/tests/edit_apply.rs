use dbento_core::{
    ConnectionSpec, Edit, Engine, MemoryStore, Secret, SecretKey, SecretStore, Workspace,
    WriteError, move_secret, plan_edit, secret_env_var,
};
use std::path::Path;

const TEXT: &str =
    "dbento = 1\r\n# kept\r\n[connections.lite]\r\nengine = \"sqlite\"\r\npath = \"a.db\"\r\n";

fn ws(text: &str) -> (tempfile::TempDir, Workspace) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("dbento.toml"), text).unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    (dir, ws)
}

fn add(ws: &Workspace) -> dbento_core::EditPlan {
    let mut s = ConnectionSpec::new(Engine::Sqlite);
    s.path = Some("b.db".into());
    plan_edit(
        ws,
        &Edit::PutConnection {
            previous: None,
            name: "b".into(),
            spec: s,
        },
    )
    .unwrap()
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    v.sort();
    v
}

#[test]
fn apply_writes_the_new_text_and_nothing_else() {
    let (d, w) = ws(TEXT);
    let plan = add(&w);
    plan.apply().unwrap();
    let file = d.path().join("dbento.toml");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), plan.new_text());
    assert_eq!(names(d.path()), ["dbento.toml"]);
    let again = Workspace::open(d.path()).unwrap();
    assert!(again.declared("b").is_some());
    assert_eq!(again.source_text(), plan.new_text());
}

#[test]
fn apply_refuses_a_file_changed_since_load() {
    let (d, w) = ws(TEXT);
    let plan = add(&w);
    let file = d.path().join("dbento.toml");
    let edited = format!("{TEXT}# by hand\r\n");
    std::fs::write(&file, &edited).unwrap();
    assert_eq!(plan.apply(), Err(WriteError::Changed));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), edited);
}

#[cfg(unix)]
#[test]
fn apply_refuses_a_symlinked_file_and_keeps_its_permissions_otherwise() {
    use std::os::unix::fs::PermissionsExt;
    let (d, w) = ws(TEXT);
    let file = d.path().join("dbento.toml");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    add(&w).apply().unwrap();
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );

    let (d, _) = ws(TEXT);
    let real = d.path().join("real.toml");
    std::fs::rename(d.path().join("dbento.toml"), &real).unwrap();
    std::os::unix::fs::symlink(&real, d.path().join("dbento.toml")).unwrap();
    let w = Workspace::open(d.path()).unwrap();
    assert!(matches!(add(&w).apply(), Err(WriteError::Symlink { .. })));
    assert_eq!(std::fs::read_to_string(&real).unwrap(), TEXT);
}

#[test]
fn a_rename_moves_the_saved_queries_folder() {
    let (d, w) = ws(TEXT);
    std::fs::create_dir_all(d.path().join("queries/lite")).unwrap();
    std::fs::write(d.path().join("queries/lite/top.sql"), "SELECT 1").unwrap();
    let plan = plan_edit(
        &w,
        &Edit::PutConnection {
            previous: Some("lite".into()),
            name: "lite2".into(),
            spec: w.connection_spec("lite").unwrap(),
        },
    )
    .unwrap();
    plan.apply().unwrap();
    plan.move_queries().unwrap();
    assert_eq!(
        std::fs::read_to_string(d.path().join("queries/lite2/top.sql")).unwrap(),
        "SELECT 1"
    );
    assert!(!d.path().join("queries/lite").exists());

    let (_d, w) = ws(TEXT);
    let plan = plan_edit(
        &w,
        &Edit::PutConnection {
            previous: Some("lite".into()),
            name: "lite3".into(),
            spec: w.connection_spec("lite").unwrap(),
        },
    )
    .unwrap();
    plan.move_queries().unwrap();
}

#[test]
fn move_secret_moves_only_what_is_there() {
    let store = MemoryStore::new();
    let dir = Path::new("/w");
    let (old, new) = (SecretKey::new(dir, "lite"), SecretKey::new(dir, "lite2"));
    assert!(!move_secret(&store, &old, &new).unwrap());
    store.set(&old, &Secret::new("pw")).unwrap();
    assert!(move_secret(&store, &old, &new).unwrap());
    assert_eq!(store.get(&new).unwrap().unwrap().expose(), "pw");
    assert!(store.get(&old).unwrap().is_none());
    assert_eq!(secret_env_var("my-db"), "DBENTO_SECRET_MY_DB");
}

#[test]
fn move_secret_onto_its_own_key_keeps_it() {
    let store = MemoryStore::new();
    let key = SecretKey::new(Path::new("/w"), "lite");
    assert!(!move_secret(&store, &key, &key).unwrap());
    store.set(&key, &Secret::new("pw")).unwrap();
    assert!(move_secret(&store, &key, &key).unwrap());
    assert_eq!(store.get(&key).unwrap().unwrap().expose(), "pw");
}

#[test]
fn same_session_ignores_only_colour_and_folder() {
    let base = "dbento = 1\n[connections.p]\nengine = \"postgres\"\nhost = \"localhost\"\ndatabase = \"d\"\nuser = \"u\"\n";
    let (_a, a) = ws(base);
    let pa = a.declared("p").unwrap();
    for (extra, same) in [
        ("color = \"red\"\nfolder = \"x\"\n", true),
        ("tls = \"off\"\n", true),
        ("port = 5432\n", true),
        ("port = 5433\n", false),
        ("read_only = true\n", false),
        ("statement_timeout = \"5s\"\n", false),
        ("secret = \"none\"\n", false),
        ("settings = { search_path = \"app\" }\n", false),
    ] {
        let (_b, b) = ws(&format!("{base}{extra}"));
        assert_eq!(pa.same_session(b.declared("p").unwrap()), same, "{extra}");
    }
}

#[cfg(unix)]
#[test]
fn write_errors_name_the_file_relative_to_the_workspace() {
    let (d, _) = ws(TEXT);
    let real = d.path().join("real.toml");
    std::fs::rename(d.path().join("dbento.toml"), &real).unwrap();
    std::os::unix::fs::symlink(&real, d.path().join("dbento.toml")).unwrap();
    let w = Workspace::open(d.path()).unwrap();
    assert_eq!(
        add(&w).apply().unwrap_err().to_string(),
        "dbento.toml is a symbolic link; edit it by hand"
    );
}
