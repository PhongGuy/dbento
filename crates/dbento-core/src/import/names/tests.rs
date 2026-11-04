//! Titles and folder names to portable file names.
use super::*;

#[test]
fn unsafe_characters_become_underscores_and_sql_is_appended() {
    assert_eq!(file_name("Top orders"), "Top orders.sql");
    assert_eq!(
        file_name(r#"a/b\c:d*e?f"g<h>i|j"#),
        "a_b_c_d_e_f_g_h_i_j.sql"
    );
    assert_eq!(file_name("tab\there\u{7}"), "tab_here_.sql");
}

#[test]
fn dots_and_spaces_are_trimmed_and_empty_names_fall_back_to_query() {
    assert_eq!(file_name("  .hidden. "), "hidden.sql");
    for t in ["", ".", "..", "   ", " . "] {
        assert_eq!(file_name(t), "query.sql", "{t:?}");
    }
}

#[test]
fn reserved_windows_names_are_prefixed() {
    for (t, want) in [
        ("CON", "_CON.sql"),
        ("nul", "_nul.sql"),
        ("Com1", "_Com1.sql"),
        ("lpt9.txt", "_lpt9.txt.sql"),
        ("CONSOLE", "CONSOLE.sql"),
        ("COM0", "COM0.sql"),
    ] {
        assert_eq!(file_name(t), want, "{t:?}");
    }
}

#[test]
fn a_title_already_ending_in_sql_keeps_one_extension() {
    assert_eq!(file_name("report.sql"), "report.sql");
    assert_eq!(file_name("REPORT.SQL"), "REPORT.SQL");
}

#[test]
fn long_names_are_cut_to_200_bytes_on_a_char_boundary() {
    assert_eq!(
        file_name(&"x".repeat(300)),
        format!("{}.sql", "x".repeat(200))
    );
    assert_eq!(
        file_name(&"é".repeat(150)),
        format!("{}.sql", "é".repeat(100))
    );
    let dotted = format!("{}. tail", "y".repeat(199));
    assert_eq!(file_name(&dotted), format!("{}.sql", "y".repeat(199)));
}

#[test]
fn folder_segments_follow_the_same_rule_without_an_extension() {
    assert_eq!(segment("Monthly / Q?"), "Monthly _ Q_");
    assert_eq!(segment(".."), "query");
    assert_eq!(segment("AUX"), "_AUX");
    assert_eq!(segment("Reports"), "Reports");
}

#[test]
fn a_cut_that_leaves_a_reserved_name_is_still_prefixed() {
    let padded = format!("CON{}x", " ".repeat(197));
    assert_eq!(file_name(&padded), "_CON.sql");
    assert_eq!(segment(&padded), "_CON");
}
