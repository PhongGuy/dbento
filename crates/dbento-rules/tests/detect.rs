use crate::common::RULES;

fn detect(bytes: &[u8]) -> Option<String> {
    RULES.detect_format(bytes)
}

#[test]
fn the_seed_declares_format_1() {
    assert_eq!(
        detect(dbento_workspace::TEMPLATE.as_bytes()).as_deref(),
        Some("1")
    );
}

#[test]
fn bom_crlf_and_comments_are_read_as_the_parser_reads_them() {
    for text in [
        "\u{feff}dbento = 1\n",
        "dbento = 1\r\n\r\n[connections.a]\r\nengine = \"sqlite\"\r\n",
        "# top\ndbento = 1 # trailing\n",
    ] {
        assert_eq!(detect(text.as_bytes()).as_deref(), Some("1"), "{text:?}");
    }
}

#[test]
fn any_root_integer_is_returned_as_its_decimal_string() {
    for (text, want) in [
        ("dbento = 2\n", "2"),
        ("dbento = -3\n", "-3"),
        ("dbento = 0x10\n", "16"),
    ] {
        assert_eq!(detect(text.as_bytes()).as_deref(), Some(want), "{text:?}");
    }
}

#[test]
fn no_root_integer_dbento_is_none() {
    for text in [
        "dbento = \"1\"\n",
        "dbento = 1.0\n",
        "x = 1\n",
        "",
        "[connections.a]\ndbento = 1\n",
        "dbento = 1\n[a\n",
        "dbento = 1\ndbento = 2\n",
    ] {
        assert_eq!(detect(text.as_bytes()), None, "{text:?}");
    }
    assert_eq!(detect(b"dbento = 1\n# \xff\n"), None);
}
