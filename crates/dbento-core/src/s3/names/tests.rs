// Through the crate root, so the public re-export is what is tested.
use crate::{display_key, is_hidden_char, suggested_file_name};

#[test]
fn plain_keys_display_unchanged() {
    for key in [
        "logs/2026/11/23.json",
        "日本/語.txt",
        "a b/c-d_e.f",
        "émoji 🍱",
    ] {
        assert_eq!(display_key(key), key, "{key:?}");
    }
}

#[test]
fn controls_line_breaks_and_bidi_marks_display_escaped() {
    assert_eq!(display_key("a\nb"), "a\\u{a}b");
    assert_eq!(display_key("\u{1b}[31mred"), "\\u{1b}[31mred");
    assert_eq!(display_key("x\u{7f}\u{85}\u{9f}"), "x\\u{7f}\\u{85}\\u{9f}");
    assert_eq!(display_key("\u{202e}gpj.exe"), "\\u{202e}gpj.exe");
    assert_eq!(
        display_key("\u{2066}\u{2069}\u{200e}\u{200f}\u{61c}\u{2028}\u{2029}"),
        "\\u{2066}\\u{2069}\\u{200e}\\u{200f}\\u{61c}\\u{2028}\\u{2029}"
    );
    assert_eq!(display_key("tab\there\r"), "tab\\u{9}here\\u{d}");
}

#[test]
fn a_backslash_displays_doubled_so_no_key_can_mimic_an_escape() {
    assert_eq!(display_key("a\\u{a}b"), "a\\\\u{a}b");
    assert_ne!(display_key("a\\u{a}b"), display_key("a\nb"));
    assert_eq!(display_key("dir\\file"), "dir\\\\file");
}

#[test]
fn a_file_name_is_the_last_segment_of_the_key() {
    assert_eq!(suggested_file_name("logs/2026/report.csv"), "report.csv");
    assert_eq!(suggested_file_name("report.csv"), "report.csv");
    assert_eq!(suggested_file_name("photos/2026/"), "2026");
    assert_eq!(suggested_file_name("a//b//"), "b");
    assert_eq!(suggested_file_name("日本/語.txt"), "語.txt");
}

#[test]
fn separators_controls_and_bidi_marks_are_dropped() {
    assert_eq!(suggested_file_name("a/..\\..\\evil.sh"), "evil.sh");
    assert_eq!(suggested_file_name("x/C:evil"), "Cevil");
    assert_eq!(suggested_file_name("x/a\nb\tc\u{1b}"), "abc");
    assert_eq!(suggested_file_name("x/\u{202e}gpj.exe"), "gpj.exe");
    assert_eq!(suggested_file_name("x/a<b>c\"d|e?f*g"), "abcdefg");
}

#[test]
fn dots_and_spaces_at_either_end_are_trimmed() {
    assert_eq!(suggested_file_name("a/.."), "download");
    assert_eq!(suggested_file_name("a/."), "download");
    assert_eq!(suggested_file_name("a/.bashrc"), "bashrc");
    assert_eq!(suggested_file_name("a/ name. . "), "name");
    assert_eq!(suggested_file_name("a/...x..."), "x");
}

#[test]
fn nothing_left_falls_back_to_download() {
    for key in ["", "/", "///", "a/\u{1b}\u{202e}", "a/::", ".."] {
        assert_eq!(suggested_file_name(key), "download", "{key:?}");
    }
}

#[test]
fn a_windows_device_name_gets_a_leading_underscore() {
    for (key, want) in [
        ("x/CON", "_CON"),
        ("x/nul.txt", "_nul.txt"),
        ("x/Com1.log", "_Com1.log"),
        ("x/lpt9", "_lpt9"),
        ("x/COM0", "COM0"),
        ("x/CONSOLE", "CONSOLE"),
        ("x/com10", "com10"),
    ] {
        assert_eq!(suggested_file_name(key), want, "{key:?}");
    }
}

#[test]
fn a_long_name_is_cut_to_200_bytes_on_a_char_boundary() {
    let long = format!("x/{}", "é".repeat(150));
    let name = suggested_file_name(&long);
    assert_eq!(name.len(), 200);
    assert!(name.chars().all(|c| c == 'é'));
    let odd = format!("x/a{}", "é".repeat(150));
    assert_eq!(suggested_file_name(&odd).len(), 199);
    let ascii = format!("x/{}", "a".repeat(300));
    assert_eq!(suggested_file_name(&ascii).len(), 200);
}

#[test]
fn a_long_device_name_stays_within_201_bytes() {
    let key = format!("x/CON.{}", "a".repeat(251));
    let name = suggested_file_name(&key);
    assert!(name.starts_with("_CON."), "{name}");
    assert_eq!(name.len(), 201);
}

#[test]
fn superscript_and_console_device_names_are_reserved_too() {
    for name in ["COM¹", "lpt²", "COM³.txt", "CONIN$", "conout$.log"] {
        let got = suggested_file_name(&format!("dir/{name}"));
        assert_ne!(
            got.to_ascii_lowercase(),
            name.to_ascii_lowercase(),
            "{name}"
        );
    }
    assert_eq!(suggested_file_name("dir/COM10"), "COM10");
}

#[test]
fn the_hidden_predicate_is_the_set_display_key_escapes() {
    for c in [
        '\n', '\u{1b}', '\u{7f}', '\u{85}', '\u{9f}', '\u{61c}', '\u{200e}', '\u{2028}',
        '\u{202e}', '\u{2066}', '\u{2069}',
    ] {
        assert!(is_hidden_char(c), "{c:?} is hidden");
        assert_ne!(
            display_key(&c.to_string()),
            c.to_string(),
            "{c:?} is escaped"
        );
    }
    for c in ['a', 'é', '日', '🍱', ' ', '\\', '\''] {
        assert!(!is_hidden_char(c), "{c:?} is shown");
    }
}
