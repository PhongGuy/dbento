#[test]
fn the_crate_reports_its_version() {
    assert_eq!(dbento_core::VERSION, env!("CARGO_PKG_VERSION"));
    assert_eq!(dbento_core::VERSION, "0.1.0");
}
