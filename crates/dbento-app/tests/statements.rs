//! The editor's statement index: the statement around the caret, re-split only near an edit.
use dbento_app::completion::statement_base;
use dbento_app::statements::StatementIndex;
use dbento_core::{Engine, statement_ranges};
use gpui_kit::component::input::Rope;

/// The base a full split of `text` gives at `offset`.
fn full(engine: Engine, text: &str, offset: usize) -> std::ops::Range<usize> {
    statement_base(&statement_ranges(engine, text), offset, text.len())
}

const SQLITE: &str = "SELECT 1;\n-- a comment; still a comment\nSELECT 'a;b' FROM t;\n\n\
    CREATE TRIGGER tr AFTER INSERT ON t BEGIN UPDATE t SET x = 1; END;\nSELECT x FROM ";
const PG: &str = "SELECT 1; SELECT $$ a ; b $$ FROM t; /* x; /* y; */ z; */ SELECT \"a;\" FROM u;\n\
    SELECT x FROM ";

// Typing, deleting and jumping the caret around must find what a full split finds.
#[test]
fn the_index_agrees_with_a_full_split_through_edits() {
    for (engine, sql) in [(Engine::Sqlite, SQLITE), (Engine::Postgres, PG)] {
        let mut ix = StatementIndex::new(engine);
        let mut text = sql.to_owned();
        let mut check = |text: &str| {
            let rope = Rope::from(text);
            for offset in (0..=text.len()).filter(|&o| text.is_char_boundary(o)) {
                assert_eq!(
                    ix.around(&rope, offset),
                    full(engine, text, offset),
                    "{engine:?} at {offset} in {text:?}"
                );
            }
        };
        check(&text);
        for c in "orders; SELECT 2".chars() {
            text.push(c);
            check(&text);
        }
        text.insert_str(10, "UPDATE t SET y = 2;");
        check(&text);
        text.replace_range(0..3, "");
        check(&text);
    }
}

// Typing on at the end of an unfinished statement keeps it one statement.
#[test]
fn typing_at_the_end_extends_the_last_statement() {
    let mut ix = StatementIndex::new(Engine::Sqlite);
    let mut text = "SELECT 1; SELECT * FROM A".to_owned();
    ix.around(&Rope::from(text.as_str()), text.len());
    text.push('l');
    let at = text.len();
    assert_eq!(
        ix.around(&Rope::from(text.as_str()), at),
        full(Engine::Sqlite, &text, at)
    );
}

// A keystroke in the middle of a big buffer splits the statements around it, not the buffer.
#[test]
fn a_keystroke_splits_only_near_the_caret() {
    let stmt = "INSERT INTO t VALUES (1, 'some text; with a semicolon');\n";
    let mut text = stmt.repeat(20_000);
    let caret = text.len() / 2 - text.len() / 2 % stmt.len() + 12;
    let mut ix = StatementIndex::new(Engine::Sqlite);
    ix.around(&Rope::from(text.as_str()), caret);
    text.insert(caret, 'x');
    let rope = Rope::from(text.as_str());
    assert_eq!(
        ix.around(&rope, caret + 1),
        full(Engine::Sqlite, &text, caret + 1)
    );
    assert!(
        ix.last_split_bytes() < 64 * 1024,
        "split {} of {} bytes",
        ix.last_split_bytes(),
        text.len()
    );
}

/// Times a keystroke's completion in the middle of a 5 MB buffer: the old path copies the rope
/// and splits it whole, the index splits near the caret. Run with `--ignored --nocapture`.
#[test]
#[ignore]
fn time_a_keystroke_in_a_large_buffer() {
    use dbento_app::completion::{complete, complete_in};
    use dbento_app::schema_tree::CatalogTree;
    use std::time::Instant;
    let stmt = "INSERT INTO t VALUES (1, 'some text; with a semicolon', 42);\n";
    let mut text = stmt.repeat(5 * 1024 * 1024 / stmt.len());
    let mut caret = text.len() / 2 - text.len() / 2 % stmt.len();
    text.insert_str(caret, "SELECT  FROM t;\n");
    caret += 7;
    let tree = CatalogTree::new();
    let mut ix = StatementIndex::new(Engine::Sqlite);
    ix.around(&Rope::from(text.as_str()), caret);
    let (mut old, mut new, mut split) = (Vec::new(), Vec::new(), 0);
    for c in "abcdefghijklmnopqrstuvwxyz".chars().cycle().take(50) {
        text.insert(caret, c);
        caret += 1;
        let rope = Rope::from(text.as_str());
        let t = Instant::now();
        let whole = rope.to_string();
        let a = complete(Engine::Sqlite, &whole, caret, &tree, "c", Some("main"));
        old.push(t.elapsed());
        let t = Instant::now();
        let base = ix.around(&rope, caret);
        let piece = rope.slice(base.clone()).to_string();
        let b = complete_in(
            Engine::Sqlite,
            &piece,
            base.start,
            caret,
            &tree,
            "c",
            Some("main"),
        );
        new.push(t.elapsed());
        split = split.max(ix.last_split_bytes());
        assert_eq!(a, b);
    }
    let med = |v: &mut Vec<std::time::Duration>| {
        v.sort();
        v[v.len() / 2]
    };
    println!(
        "buffer {} bytes; median per keystroke: whole {:?}, index {:?}; index split at most {split} bytes",
        text.len(),
        med(&mut old),
        med(&mut new)
    );
}
