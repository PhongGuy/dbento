//! The forest grammar in both forms and the folder cleaning rules.
use super::*;

const V2: &str = "\n  1:0:8062b3db-0000-4000-8000-000000000001:Prod\n  2:1:8062b3db-0000-4000-8000-000000000002:EU:west\n  ----------------------------------------\n  3:2:ds-a\n  4:1:ds-b\n  5:0:ds-root\n";
const LEGACY: &str = "1:0:MiniServices\n2:1:Control Plane\n----------------------------------------\n3:2:ds-a\n4:0:ds-root\n";

fn path(m: &HashMap<String, Arc<[String]>>, ds: &str) -> Option<Vec<String>> {
    m.get(ds).map(|p| p.to_vec())
}

#[test]
fn v2_lines_build_nested_paths_and_root_sources_have_none() {
    let m = parse_forest(V2, ForestForm::V2, |_| true);
    assert_eq!(
        path(&m, "ds-a"),
        Some(vec!["Prod".into(), "EU:west".into()])
    );
    assert_eq!(path(&m, "ds-b"), Some(vec!["Prod".into()]));
    assert_eq!(path(&m, "ds-root"), None);
}

#[test]
fn legacy_folder_lines_have_no_uuid() {
    let m = parse_forest(LEGACY, ForestForm::Legacy, |_| true);
    assert_eq!(
        path(&m, "ds-a"),
        Some(vec!["MiniServices".into(), "Control Plane".into()])
    );
    assert_eq!(path(&m, "ds-root"), None);
}

#[test]
fn cycles_dangling_parents_and_garbled_lines_drop_only_their_sources() {
    let data = "1:2:Loop\n2:1:Back\nx:0:Bad\n3:0:Ok\n----\n10:1:ds-cycle\n11:9:ds-dangling\n12:3:ds-ok\nnonsense\n";
    let m = parse_forest(data, ForestForm::Legacy, |_| true);
    assert_eq!(path(&m, "ds-cycle"), None);
    assert_eq!(path(&m, "ds-dangling"), None);
    assert_eq!(path(&m, "ds-ok"), Some(vec!["Ok".into()]));
    assert_eq!(m.len(), 1);
}

#[test]
fn cleaning_strips_invisible_and_control_characters_and_empty_segments() {
    let f = |segs: &[&str]| clean_folder(segs.iter().copied()).map(|p| p.to_string());
    assert_eq!(f(&["\u{200B}backend"]), Some("backend".into()));
    assert_eq!(f(&["\u{FEFF} Prod ", "", "E\tU"]), Some("Prod/EU".into()));
    assert_eq!(f(&["\u{200B}"]), None);
    assert_eq!(f(&[]), None);
}

#[test]
fn a_forest_path_wins_over_group_and_group_nests_on_slash() {
    let forest = vec!["Prod".to_owned(), "EU".to_owned()];
    let s = |f: Option<&[String]>, g: Option<&str>| source_folder(f, g).map(|p| p.to_string());
    assert_eq!(s(Some(&forest), Some("Other")), Some("Prod/EU".into()));
    assert_eq!(
        s(None, Some("MiniServices/Control Plane")),
        Some("MiniServices/Control Plane".into())
    );
    assert_eq!(s(Some(&[]), Some("dev")), Some("dev".into()));
    assert_eq!(
        s(Some(&["\u{200B}".to_owned()]), Some("dev")),
        Some("dev".into())
    );
    assert_eq!(s(None, Some("//")), None);
    assert_eq!(s(None, None), None);
}

/// A forest of `n` chained folders (`1` at the root, `k` under `k - 1`) followed by
/// `sources` data sources that all sit in the deepest folder.
fn chain(n: u64, sources: u64) -> String {
    let mut data: String = (1..=n).map(|k| format!("{k}:{}:f\n", k - 1)).collect();
    data.push_str("----\n");
    data.extend((0..sources).map(|s| format!("{}:{n}:ds-{s}\n", n + 1 + s)));
    data
}

#[test]
fn a_chain_deeper_than_the_cap_gives_no_folder_and_stays_linear() {
    let at_cap = parse_forest(&chain(MAX_DEPTH as u64, 1), ForestForm::Legacy, |_| true);
    assert_eq!(at_cap["ds-0"].len(), MAX_DEPTH);
    // Without a depth cap this keeps sources x depth strings and runs out of memory.
    let deep = parse_forest(&chain(200_000, 200_000), ForestForm::Legacy, |_| true);
    assert!(deep.is_empty());
}

#[test]
fn a_long_cycle_gives_no_folder() {
    let n = 200_000u64;
    let mut data: String = (1..=n).map(|k| format!("{k}:{}:f\n", k % n + 1)).collect();
    data.push_str("----\n");
    data.extend((0..n).map(|s| format!("{}:1:ds-{s}\n", n + 1 + s)));
    assert!(parse_forest(&data, ForestForm::Legacy, |_| true).is_empty());
}

#[test]
fn slashes_inside_a_name_nest_and_stray_slashes_are_dropped() {
    let f = |segs: &[&str]| clean_folder(segs.iter().copied()).map(|p| p.to_string());
    assert_eq!(f(&["Prod / EU"]), Some("Prod/EU".into()));
    assert_eq!(f(&["Prod/", "x"]), Some("Prod/x".into()));
}

/// One folder `1` at the root named `name`, holding `sources` data sources.
fn one_folder(name: &str, sources: u64) -> String {
    let mut data = format!("1:0:{name}\n----\n");
    data.extend((0..sources).map(|s| format!("{}:1:ds-{s}\n", s + 2)));
    data
}

#[test]
fn a_huge_folder_name_gives_no_folder_without_copying_it_per_source() {
    // Without a byte cap and sharing, every source keeps its own 8 MB copy.
    let m = parse_forest(
        &one_folder(&"n".repeat(8 << 20), 100_000),
        ForestForm::Legacy,
        |_| true,
    );
    assert!(m.is_empty());
}

#[test]
fn a_path_up_to_the_byte_cap_is_kept_and_one_byte_more_is_dropped() {
    let half = MAX_PATH_BYTES / 2;
    let mut at = format!("1:0:{}\n", "a".repeat(half));
    at.push_str(&format!(
        "2:1:{}\n----\n3:2:ds\n",
        "b".repeat(MAX_PATH_BYTES - half)
    ));
    assert!(parse_forest(&at, ForestForm::Legacy, |_| true).contains_key("ds"));
    let over = one_folder(&"a".repeat(MAX_PATH_BYTES + 1), 1);
    assert!(parse_forest(&over, ForestForm::Legacy, |_| true).is_empty());
}

#[test]
fn ids_with_a_sign_are_garbled_lines() {
    let data = "+1:0:Signed\n2:+0:Parent\n3:0:Ok\n----\n4:1:ds-a\n5:2:ds-b\n+6:3:ds-c\n7:3:ds-d\n";
    let m = parse_forest(data, ForestForm::Legacy, |_| true);
    assert_eq!(path(&m, "ds-a"), None);
    assert_eq!(path(&m, "ds-b"), None);
    assert_eq!(path(&m, "ds-c"), None);
    assert_eq!(path(&m, "ds-d"), Some(vec!["Ok".into()]));
}

#[test]
fn only_the_sources_asked_for_are_kept() {
    let m = parse_forest(V2, ForestForm::V2, |uuid| uuid == "ds-b");
    assert_eq!(m.len(), 1);
    assert_eq!(path(&m, "ds-b"), Some(vec!["Prod".into()]));
}

#[test]
fn a_forest_with_more_folders_than_the_cap_gives_no_folders() {
    let at_cap = chain(MAX_FOLDERS as u64, 1).replacen("1:0:f\n", "1:0:top\n", 1);
    let shallow = format!("{at_cap}{}:1:ds-top\n", MAX_FOLDERS + 2);
    assert!(parse_forest(&shallow, ForestForm::Legacy, |_| true).contains_key("ds-top"));
    let over = chain(MAX_FOLDERS as u64 + 1, 0) + &format!("{}:1:ds-top\n", MAX_FOLDERS + 2);
    assert!(parse_forest(&over, ForestForm::Legacy, |_| true).is_empty());
}
