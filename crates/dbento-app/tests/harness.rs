//! Keeps every app test window on the reduced-motion path in `support::init`.

use std::fs;
use std::path::Path;

// A test that inits gpui-kit or opens a window by itself would run wall-clock animations,
// so scan the test sources: `gpui_kit::init` lives only in support, and a file that opens a
// window must call `support::init`.
#[test]
fn every_test_window_goes_through_support_init() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut offenders = Vec::new();
    for entry in fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "rs") || path.ends_with("harness.rs") {
            continue;
        }
        let src = fs::read_to_string(&path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if src.contains("gpui_kit::init") {
            offenders.push(format!("{name}: calls gpui_kit::init; use support::init"));
        }
        if src.contains("open_window") && !src.contains("support::init(") {
            offenders.push(format!("{name}: opens a window without support::init"));
        }
    }
    assert!(offenders.is_empty(), "{offenders:#?}");
}
