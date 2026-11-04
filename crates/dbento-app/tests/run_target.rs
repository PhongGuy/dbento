use dbento_app::run_target::{run_range, statement_at};
use dbento_core::Engine::{Postgres, Sqlite};

#[test]
fn the_caret_picks_the_statement_on_its_line() {
    let sql = "SELECT 1;\nSELECT 2;";
    for engine in [Sqlite, Postgres] {
        assert_eq!(statement_at(engine, sql, 0), Some(0..9));
        assert_eq!(
            statement_at(engine, sql, 9),
            Some(0..9),
            "right after the semicolon"
        );
        assert_eq!(statement_at(engine, sql, 10), Some(9..19));
        assert_eq!(statement_at(engine, sql, 19), Some(9..19));
    }
}

#[test]
fn blank_space_belongs_to_the_statement_on_its_line_else_the_next() {
    let sql = "SELECT 1;  \n\nSELECT 2;";
    for engine in [Sqlite, Postgres] {
        assert_eq!(statement_at(engine, sql, 10), Some(0..9));
        assert_eq!(statement_at(engine, sql, 12), Some(9..22));
    }
    assert_eq!(statement_at(Sqlite, "SELECT 1;\n\n", 11), Some(0..9));
    assert_eq!(statement_at(Sqlite, "SELECT 1", 3), Some(0..8));
    assert_eq!(statement_at(Sqlite, "", 0), None);
    assert_eq!(statement_at(Sqlite, "  -- c", 3), None);
}

#[test]
fn a_comment_belongs_by_the_newline_rule() {
    for engine in [Sqlite, Postgres] {
        // On the line of the first statement: the first statement.
        assert_eq!(
            statement_at(engine, "SELECT 1; -- c\nSELECT 2;", 12),
            Some(0..9)
        );
        // On a line of its own before the second: the second, with its comment.
        assert_eq!(
            statement_at(engine, "SELECT 1;\n-- next\nSELECT 2;", 12),
            Some(9..27)
        );
    }
    assert_eq!(
        statement_at(Sqlite, "SELECT 1; /* c */ SELECT 2;", 12),
        Some(0..9)
    );
}

#[test]
fn a_selection_runs_as_is_and_a_blank_one_falls_back_to_the_caret() {
    let sql = "SELECT 1; SELECT 2;";
    assert_eq!(run_range(Sqlite, sql, 10..19), Some(10..19));
    assert_eq!(run_range(Sqlite, sql, 0..8), Some(0..8));
    assert_eq!(run_range(Sqlite, sql, 9..10), Some(0..9));
    assert_eq!(
        run_range(Postgres, "SELECT $$;$$; SELECT 2", 2..2),
        Some(0..13)
    );
    assert_eq!(run_range(Sqlite, "", 0..0), None);
}

#[cfg(not(target_os = "macos"))]
#[test]
fn key_hints_spell_the_bindings() {
    use dbento_app::keymap::{RUN_ALL_KEYS, RUN_STATEMENT_KEYS, key_hint};
    assert_eq!(key_hint(RUN_STATEMENT_KEYS), ["Ctrl", "\u{23CE}"]);
    assert_eq!(key_hint(RUN_ALL_KEYS), ["Ctrl", "Shift", "\u{23CE}"]);
}

#[cfg(not(target_os = "macos"))]
#[test]
fn arrow_keys_hint_as_arrows_and_other_keys_stay_upper_case() {
    use dbento_app::keymap::{NAV_BACK_KEYS, NAV_FORWARD_KEYS, key_hint, plus_keys};
    assert_eq!(plus_keys(NAV_BACK_KEYS), "Alt+\u{2190}");
    assert_eq!(plus_keys(NAV_FORWARD_KEYS), "Alt+\u{2192}");
    assert_eq!(key_hint("ctrl-delete"), ["Ctrl", "DELETE"]);
    assert_eq!(key_hint("f2"), ["F2"]);
}
