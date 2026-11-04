//! DataGrip folders become `folder` on drafts; data-source colours become tags.
use dbento_core::Color;
use dbento_core::import::WarningKind;
use dbento_core::import::datagrip::{DataGripOptions, read_datagrip};
use std::path::Path;

fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().unwrap();
    for (rel, text) in files {
        let path = dir.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    dir
}

fn source(name: &str, uuid: &str, group: Option<&str>) -> String {
    let g = group.map(|g| format!(" group=\"{g}\"")).unwrap_or_default();
    format!(
        "<data-source source=\"LOCAL\" name=\"{name}\" uuid=\"{uuid}\"{g}>\
         <driver-ref>sqlite.xerial</driver-ref>\
         <jdbc-url>jdbc:sqlite:$PROJECT_DIR$/{name}.db</jdbc-url></data-source>"
    )
}

fn sources(items: &[String]) -> String {
    format!(
        "<project version=\"4\"><component name=\"DataSourceManagerImpl\">{}</component></project>",
        items.concat()
    )
}

fn folders(dir: &Path) -> Vec<(String, Option<String>)> {
    let set = read_datagrip(dir, &DataGripOptions::new()).unwrap();
    set.connections()
        .iter()
        .map(|c| {
            (
                c.name().to_owned(),
                c.draft().folder().map(|f| f.to_string()),
            )
        })
        .collect()
}

const V2_FOREST: &str = "<project version=\"4\"><component name=\"db-forest-configuration\"><data version=\"2\">\
1:0:8062b3db-0000-4000-8000-000000000001:\u{200B}Prod\n\
2:1:8062b3db-0000-4000-8000-000000000002:EU\n\
----------------------------------------\n\
3:2:u1\n</data></component></project>";

const LEGACY_FOREST: &str = "<project version=\"4\"><component name=\"db-tree-configuration\">\
<option name=\"data\" value=\"1:0:Ops&#10;2:1:Deep&#10;----------------------------------------&#10;3:2:u1&#10;\" />\
</component></project>";

#[test]
fn group_attributes_nest_on_slash_and_lose_their_zero_width_prefix() {
    let xml = sources(&[
        source("a", "u1", Some("Prod/EU")),
        source("b", "u2", Some("\u{200B}backend")),
        source("c", "u3", None),
    ]);
    let d = project(&[(".idea/dataSources.xml", &xml)]);
    assert_eq!(
        folders(d.path()),
        [
            ("a".to_owned(), Some("Prod/EU".to_owned())),
            ("b".to_owned(), Some("backend".to_owned())),
            ("c".to_owned(), None),
        ]
    );
}

#[test]
fn a_v2_or_legacy_forest_path_beats_the_group_attribute() {
    let xml = sources(&[
        source("a", "u1", Some("Other")),
        source("b", "u2", Some("dev")),
    ]);
    for (forest, want) in [(V2_FOREST, "Prod/EU"), (LEGACY_FOREST, "Ops/Deep")] {
        let d = project(&[
            (".idea/dataSources.xml", &xml),
            (".idea/db-forest-config.xml", forest),
        ]);
        assert_eq!(
            folders(d.path()),
            [
                ("a".to_owned(), Some(want.to_owned())),
                ("b".to_owned(), Some("dev".to_owned()))
            ]
        );
    }
}

#[test]
fn a_malformed_forest_file_falls_back_to_groups_without_failing() {
    let xml = sources(&[source("a", "u1", Some("dev"))]);
    let d = project(&[
        (".idea/dataSources.xml", &xml),
        (".idea/db-forest-config.xml", "<project><unclosed"),
    ]);
    assert_eq!(
        folders(d.path()),
        [("a".to_owned(), Some("dev".to_owned()))]
    );
}

fn colours(entries: &str) -> String {
    format!(
        "<project version=\"4\"><component name=\"DatabaseColorSettings\"><colors>{entries}</colors></component></project>"
    )
}

/// Each connection's tag and the subjects of every project-level `ColorsNotImported`.
fn tags_and_project_warnings(colors_file: Option<&str>) -> (Vec<Option<Color>>, Vec<String>) {
    let xml = sources(&[source("a", "u1", Some("dev")), source("b", "u2", None)]);
    let mut files = vec![(".idea/dataSources.xml", xml.as_str())];
    if let Some(text) = colors_file {
        files.push((".idea/databaseColors.xml", text));
    }
    let d = project(&files);
    let set = read_datagrip(d.path(), &DataGripOptions::new()).unwrap();
    let tags = set
        .connections()
        .iter()
        .map(|c| c.draft().color())
        .collect();
    let warned = set
        .warnings()
        .iter()
        .filter(|w| w.kind() == &WarningKind::ColorsNotImported)
        .map(|w| w.subject().to_owned())
        .collect();
    (tags, warned)
}

#[test]
fn groups_and_data_source_colours_never_warn() {
    assert_eq!(tags_and_project_warnings(None), (vec![None, None], vec![]));
    let only_uuids = colours(r#"<entry key="u1" value="Green" /><entry key="u2" value="BLUE" />"#);
    assert_eq!(
        tags_and_project_warnings(Some(&only_uuids)),
        (vec![Some(Color::Green), Some(Color::Blue)], vec![])
    );
}

#[test]
fn an_object_colour_keeps_one_project_warning() {
    let mixed = colours(
        r#"<entry key="u1" value="teal" /><entry key="t/public/db/u1:TABLE" value="Violet" /><entry key="s/public/db/u2:SCHEMA" value="Blue" />"#,
    );
    assert_eq!(
        tags_and_project_warnings(Some(&mixed)),
        (vec![Some(Color::Teal), None], vec!["project".to_owned()])
    );
}

#[test]
fn a_malformed_colours_file_gives_no_tags_and_the_project_warning() {
    assert_eq!(
        tags_and_project_warnings(Some("<project><unclosed")),
        (vec![None, None], vec!["project".to_owned()])
    );
}

#[test]
fn a_duplicated_uuid_never_takes_a_colour_keyed_by_it() {
    // Two sources share one uuid, so neither owns the colour entry keyed by that uuid.
    let xml = sources(&[source("a", "dup", None), source("b", "dup", None)]);
    let colors = colours(r#"<entry key="dup" value="Green" />"#);
    let d = project(&[
        (".idea/dataSources.xml", xml.as_str()),
        (".idea/databaseColors.xml", colors.as_str()),
    ]);
    let set = read_datagrip(d.path(), &DataGripOptions::new()).unwrap();
    let tags: Vec<_> = set
        .connections()
        .iter()
        .map(|c| c.draft().color())
        .collect();
    assert_eq!(tags, [None, None]);
}
