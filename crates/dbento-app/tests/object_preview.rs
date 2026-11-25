//! The object preview: which mode the bytes earn, text made safe to show, hex rows, the
//! header and note, and the S3 grid cells.
use dbento_app::grid::{cell_display, s3_cell_display};
use dbento_app::object_view::{
    ObjectView, Preview, PreviewMode, hex_line, pretty_bound, preview_mode, safe_text,
};
use dbento_app::objects::ObjectItem;
use dbento_app::record::pretty_json;
use dbento_core::{CellRef, ObjectKey};

fn meta(key: &str, size: u64) -> ObjectItem {
    ObjectItem {
        key: key.into(),
        size,
        last_modified: Some("2026-11-25T08:00:00Z".into()),
        etag: Some("abc".into()),
        storage_class: Some("STANDARD".into()),
        content_type: Some("text/plain".into()),
    }
}
fn preview(bytes: &[u8], total: u64, ct: Option<&str>) -> Preview {
    Preview {
        bytes: bytes.to_vec(),
        total_size: total,
        truncated: (bytes.len() as u64) < total,
        content_type: ct.map(Into::into),
    }
}

#[test]
fn the_mode_follows_the_bytes() {
    assert_eq!(
        preview_mode(b"hello", false, Some("application/octet-stream")),
        PreviewMode::Text
    );
    assert_eq!(preview_mode(br#"{"a":1}"#, false, None), PreviewMode::Json);
    assert_eq!(
        preview_mode(b" [1,2]", false, Some("text/plain")),
        PreviewMode::Json
    );
    assert_eq!(
        preview_mode(b"1", false, Some("application/json")),
        PreviewMode::Json
    );
    assert_eq!(
        preview_mode(br#"{"a":"#, false, Some("application/json")),
        PreviewMode::Text,
        "not JSON stays text"
    );
    assert_eq!(
        preview_mode(br#"{"a":1}"#, true, None),
        PreviewMode::Text,
        "a cut document is not parsed"
    );
    assert_eq!(
        preview_mode(b"a\0b", false, Some("text/plain")),
        PreviewMode::Hex
    );
    assert_eq!(preview_mode(&[0xff, 0xfe], false, None), PreviewMode::Hex);
    assert_eq!(
        preview_mode(&"caf\u{e9}".as_bytes()[..4], true, None),
        PreviewMode::Text,
        "a cut character is trimmed"
    );
    assert_eq!(
        preview_mode(&"caf\u{e9}".as_bytes()[..4], false, None),
        PreviewMode::Hex,
        "a whole read must be UTF-8"
    );
    assert_eq!(preview_mode(b"", false, None), PreviewMode::Text);
}

#[test]
fn text_keeps_lines_and_tabs_and_escapes_the_rest() {
    assert_eq!(safe_text("a\tb\nc"), "a\tb\nc");
    assert_eq!(safe_text("a\r\nb"), "a\nb");
    assert_eq!(safe_text("x\u{1b}[31my"), "x\\u{1b}[31my");
    assert_eq!(safe_text("lone\rcr"), "lone\\u{d}cr");
    assert_eq!(
        safe_text("p\u{202e}q\u{2066}r\u{85}s\u{2028}t"),
        "p\\u{202e}q\\u{2066}r\\u{85}s\\u{2028}t"
    );
    assert_eq!(
        safe_text("back\\slash"),
        "back\\slash",
        "content keeps its backslashes"
    );
}

#[test]
fn hex_lines_follow_hexdump() {
    assert_eq!(
        hex_line(b"hello, bento\n", 0),
        "00000000  68 65 6c 6c 6f 2c 20 62  65 6e 74 6f 0a           |hello, bento.|"
    );
    let row: Vec<u8> = (16..32).collect();
    assert_eq!(
        hex_line(&row, 16),
        "00000010  10 11 12 13 14 15 16 17  18 19 1a 1b 1c 1d 1e 1f  |................|"
    );
    assert_eq!(
        hex_line(&[0xff], 0x1f0),
        "000001f0  ff                                                |.|"
    );
}

#[test]
fn a_view_reads_once_and_words_its_header() {
    let mut v = ObjectView::new("logs", "text/hello\u{202e}.txt");
    assert_eq!(v.title(), "text/hello\\u{202e}.txt \u{b7} logs");
    assert_eq!(
        v.object_key(),
        ObjectKey::new("logs", "text/hello\u{202e}.txt")
    );
    assert!(v.begin());
    assert!(!v.begin(), "one read at a time");
    v.apply(
        meta("text/hello\u{202e}.txt", 13),
        preview(b"hello, bento\n", 13, Some("text/plain")),
    );
    assert_eq!(
        v.header().as_deref(),
        Some(
            "s3://logs/text/hello\\u{202e}.txt \u{b7} 13 bytes \u{b7} 2026-11-25T08:00:00Z \u{b7} etag abc \u{b7} STANDARD \u{b7} text/plain"
        )
    );
    assert_eq!(
        (v.mode(), v.text(), v.note()),
        (Some(PreviewMode::Text), Some("hello, bento\n"), None)
    );
}

#[test]
fn json_starts_pretty_and_switches_to_text_or_hex() {
    let doc = br#"{"name":"bento","tiers":3}"#;
    let mut v = ObjectView::new("logs", "json/doc.json");
    v.begin();
    v.apply(
        meta("json/doc.json", 26),
        preview(doc, 26, Some("application/json")),
    );
    assert_eq!(v.mode(), Some(PreviewMode::Json));
    assert_eq!(
        v.modes(),
        [PreviewMode::Text, PreviewMode::Json, PreviewMode::Hex]
    );
    assert_eq!(
        v.text(),
        Some("{\n  \"name\": \"bento\",\n  \"tiers\": 3\n}")
    );
    assert!(v.set_mode(PreviewMode::Hex));
    assert_eq!((v.text(), v.hex_rows()), (None, 2));
    assert!(v.set_mode(PreviewMode::Text));
    assert_eq!(v.text(), Some(r#"{"name":"bento","tiers":3}"#));
}

#[test]
fn bytes_that_are_not_text_show_only_as_hex_rows() {
    let bytes: Vec<u8> = (0..=255u8).collect();
    let mut v = ObjectView::new("logs", "bin/blob.bin");
    v.begin();
    v.apply(
        meta("bin/blob.bin", 256),
        preview(&bytes, 256, Some("application/octet-stream")),
    );
    assert_eq!(
        (v.mode(), v.modes()),
        (Some(PreviewMode::Hex), vec![PreviewMode::Hex])
    );
    assert!(!v.set_mode(PreviewMode::Text));
    assert!(!v.set_mode(PreviewMode::Json));
    assert_eq!((v.text(), v.hex_rows()), (None, 16));
    assert_eq!(
        v.hex_row(1).as_deref(),
        Some("00000010  10 11 12 13 14 15 16 17  18 19 1a 1b 1c 1d 1e 1f  |................|")
    );
    assert_eq!(v.hex_row(16), None);
}

#[test]
fn a_cut_read_says_how_much_shows_and_a_failure_can_be_read_again() {
    let mut v = ObjectView::new("logs", "big.txt");
    v.begin();
    v.apply(
        meta("big.txt", 3 << 20),
        preview(&vec![b'a'; 1 << 20], 3 << 20, None),
    );
    assert_eq!(v.note().as_deref(), Some("first 1.0 MiB of 3.0 MiB shown"));
    let mut gone = ObjectView::new("logs", "gone");
    gone.begin();
    gone.fail("not found: s3://logs/gone".into());
    assert_eq!(
        (gone.error(), gone.mode()),
        (Some("not found: s3://logs/gone"), None)
    );
    assert!(gone.begin(), "a failed read can be asked again");
}

#[test]
fn s3_cells_escape_a_bounded_prefix() {
    let mut big = "k".repeat(10);
    big.push('\n');
    big.push_str(&"x".repeat(1 << 20));
    let (shown, dim) = s3_cell_display(CellRef::Text(&big));
    assert!(!dim);
    assert!(
        shown.starts_with("kkkkkkkkkk\\u{a}xxx"),
        "{}",
        shown.chars().take(20).collect::<String>()
    );
    assert_eq!(shown.chars().count(), 256);
    assert_eq!(
        s3_cell_display(CellRef::Text("a\u{202e}b")).0.as_ref(),
        "a\\u{202e}b"
    );
    assert_eq!(s3_cell_display(CellRef::Null), cell_display(CellRef::Null));
    assert_eq!(
        s3_cell_display(CellRef::Bytes(b"ab")),
        cell_display(CellRef::Bytes(b"ab"))
    );
}

#[test]
fn pretty_bound_never_undercounts_the_pretty_text() {
    for doc in [
        r#"{"name":"bento","tiers":3}"#,
        r#"[ [], {}, [1, [2, {"a": [3]}]], "x,[{" ]"#,
        r#"{"a":{"b":{"c":{"d":[1,2,3,{"e":"f"}]}}}}"#,
    ] {
        let pretty = pretty_json(doc).unwrap();
        assert!(pretty_bound(doc) >= pretty.len(), "{doc}");
    }
}

#[test]
fn deeply_nested_json_shows_as_raw_text_without_being_pretty_printed() {
    // 127 levels hold half a million elements: pretty, each would carry 254 spaces of indent.
    let inner = "1,".repeat(500_000);
    let doc = format!("{}{inner}1{}", "[".repeat(127), "]".repeat(127));
    assert!(
        pretty_bound(&doc) > 4 << 20,
        "the bound sees the indent growth"
    );
    let mut v = ObjectView::new("logs", "deep.json");
    v.begin();
    let size = doc.len() as u64;
    v.apply(meta("deep.json", size), preview(doc.as_bytes(), size, None));
    assert_eq!(v.mode(), Some(PreviewMode::Json));
    assert_eq!(v.text(), Some(doc.as_str()), "the raw text, made safe");
}
