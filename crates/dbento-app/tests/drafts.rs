use dbento_app::drafts::{Draft, DraftSlot, Drafts};
use std::fs;
use std::path::Path;

fn store() -> (tempfile::TempDir, Drafts) {
    let dir = tempfile::tempdir().unwrap();
    let drafts = Drafts::new(dir.path().join("drafts"));
    (dir, drafts)
}
fn draft(id: &str, sql: &str) -> Draft {
    Draft::new(id, Path::new("/ws"), "lite", None, sql)
}
fn sqls(d: &[Draft]) -> Vec<&str> {
    d.iter().map(|d| d.sql.as_str()).collect()
}

#[test]
fn a_draft_round_trips_privately_and_leaves_no_temp_file() {
    let (dir, drafts) = store();
    let d = Draft::new(
        &Drafts::new_id(),
        Path::new("/ws"),
        "lite",
        Some(Path::new("queries/lite/a.sql")),
        "SELECT 1",
    );
    drafts.write(&d).unwrap();
    assert_eq!(drafts.load(Path::new("/ws")), vec![d.clone()]);
    let names: Vec<String> = fs::read_dir(dir.path().join("drafts"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, [format!("{}.json", d.id)]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&drafts.path_of(&d.id).unwrap()), 0o600);
        assert_eq!(mode(&dir.path().join("drafts")), 0o700);
    }
}

#[test]
fn a_draft_file_holds_only_the_known_fields() {
    let (_dir, drafts) = store();
    let d = draft(&Drafts::new_id(), "SELECT 1");
    drafts.write(&d).unwrap();
    let v: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(drafts.path_of(&d.id).unwrap()).unwrap()).unwrap();
    let mut keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "connection",
            "file",
            "id",
            "saved_at_ms",
            "sql",
            "v",
            "workspace"
        ]
    );
    assert_eq!(v["v"], 1);
}

#[test]
fn load_keeps_one_workspace_oldest_first() {
    let (_dir, drafts) = store();
    let mut a = draft("a1", "SELECT newer");
    a.saved_at_ms = 20;
    let mut b = draft("b2", "SELECT older");
    b.saved_at_ms = 10;
    let mut c = Draft::new("c3", Path::new("/other"), "lite", None, "SELECT elsewhere");
    c.saved_at_ms = 5;
    for d in [&a, &b, &c] {
        drafts.write(d).unwrap();
    }
    assert_eq!(
        sqls(&drafts.load(Path::new("/ws"))),
        ["SELECT older", "SELECT newer"]
    );
}

#[test]
fn ids_cannot_leave_the_drafts_folder() {
    let (dir, drafts) = store();
    let long = "a".repeat(65);
    for bad in ["../x", "a/b", "", "UPPER", long.as_str()] {
        assert_eq!(drafts.path_of(bad), None, "{bad}");
        let e = drafts.write(&draft(bad, "SELECT 1")).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput);
    }
    assert!(!dir.path().join("x").exists());
    let (a, b) = (Drafts::new_id(), Drafts::new_id());
    assert_ne!(a, b);
    assert!(drafts.path_of(&a).is_some());
}

#[test]
fn load_skips_corrupt_foreign_and_linked_files() {
    let (dir, drafts) = store();
    drafts.write(&draft("good", "SELECT 1")).unwrap();
    let folder = dir.path().join("drafts");
    fs::write(folder.join("broken.json"), "{").unwrap();
    fs::write(folder.join("future.json"), r#"{"v":2,"id":"future","workspace":"/ws","connection":"lite","file":null,"sql":"x","saved_at_ms":1}"#).unwrap();
    fs::write(folder.join("named.json"), r#"{"v":1,"id":"other","workspace":"/ws","connection":"lite","file":null,"sql":"x","saved_at_ms":1}"#).unwrap();
    fs::write(folder.join("notes.txt"), "hello").unwrap();
    #[cfg(unix)]
    {
        let outside = dir.path().join("outside.json");
        fs::write(&outside, r#"{"v":1,"id":"link","workspace":"/ws","connection":"lite","file":null,"sql":"x","saved_at_ms":1}"#).unwrap();
        std::os::unix::fs::symlink(&outside, folder.join("link.json")).unwrap();
    }
    assert_eq!(sqls(&drafts.load(Path::new("/ws"))), ["SELECT 1"]);
}

#[test]
fn removing_a_missing_draft_is_fine() {
    let (_dir, drafts) = store();
    drafts.remove(&Drafts::new_id()).unwrap();
}

#[test]
fn a_superseded_write_does_nothing() {
    let (_dir, drafts) = store();
    let slot = DraftSlot::new(drafts.clone(), Drafts::new_id());
    let stale = slot.write_later(draft(slot.id(), "SELECT old"));
    let remove = slot.remove_later();
    remove().unwrap();
    stale().unwrap();
    assert!(drafts.load(Path::new("/ws")).is_empty());
    let older = slot.write_later(draft(slot.id(), "SELECT 1"));
    let newer = slot.write_later(draft(slot.id(), "SELECT 2"));
    newer().unwrap();
    older().unwrap();
    assert_eq!(sqls(&drafts.load(Path::new("/ws"))), ["SELECT 2"]);
}

#[cfg(unix)]
#[test]
fn load_skips_a_fifo_without_blocking() {
    let (dir, drafts) = store();
    drafts.write(&draft("good", "SELECT 1")).unwrap();
    let fifo = dir.path().join("drafts/fifo.json");
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap();
    assert!(made.success());
    // A blocking open of a FIFO with no writer would hang, so the load runs on a thread.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || tx.send(sqls(&drafts.load(Path::new("/ws"))).len()).unwrap());
    let loaded = rx.recv_timeout(std::time::Duration::from_secs(5));
    assert_eq!(loaded, Ok(1), "loading blocked on the FIFO");
}
