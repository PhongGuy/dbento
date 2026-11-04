//! The explorer tree model: order, depth, indexes and collapse.
use dbento_app::explorer::{ExplorerRow, explorer_rows};
use dbento_core::{FolderPath, Workspace, WorkspaceItem};
use std::collections::HashSet;

const TOML: &str = r#"dbento = 1
[folders."Prod"]
color = "red"
[folders."Empty"]
[connections.zeta]
engine = "sqlite"
path = "z.db"
[connections.alpha]
engine = "sqlite"
path = "a.db"
folder = "Prod/EU"
[connections.beta]
engine = "sqlite"
path = "b.db"
folder = "Prod"
[connections.gamma]
engine = "sqlite"
path = "g.db"
folder = "Dev"
"#;

fn ws() -> (tempfile::TempDir, Workspace) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("dbento.toml"), TOML).unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    (dir, ws)
}
fn shape(rows: &[ExplorerRow]) -> Vec<String> {
    rows.iter()
        .map(|r| {
            let key = match &r.item {
                WorkspaceItem::Folder(p) => {
                    format!("{} {}", if r.expanded { "v" } else { ">" }, p.name())
                }
                WorkspaceItem::Connection(n) => n.clone(),
                other => panic!("{other:?}"),
            };
            format!("{}{key}", "  ".repeat(r.depth))
        })
        .collect()
}
fn collapsed(paths: &[&str]) -> HashSet<FolderPath> {
    paths.iter().map(|p| p.parse().unwrap()).collect()
}

#[test]
fn folders_come_first_at_every_level_and_empty_ones_show() {
    let (_d, ws) = ws();
    let rows = explorer_rows(&ws, &HashSet::new());
    assert_eq!(
        shape(&rows),
        [
            "v Dev",
            "  gamma",
            "v Empty",
            "v Prod",
            "  v EU",
            "    alpha",
            "  beta",
            "zeta"
        ]
    );
    assert_eq!(
        rows.iter().map(|r| r.index).collect::<Vec<_>>(),
        [0, 2, 1, 2, 3, 0, 1, 3]
    );
}

#[test]
fn a_collapsed_folder_hides_everything_below_it() {
    let (_d, ws) = ws();
    assert_eq!(
        shape(&explorer_rows(&ws, &collapsed(&["Prod"]))),
        ["v Dev", "  gamma", "v Empty", "> Prod", "zeta"]
    );
    assert_eq!(
        shape(&explorer_rows(&ws, &collapsed(&["Prod/EU", "Nowhere"]))),
        [
            "v Dev", "  gamma", "v Empty", "v Prod", "  > EU", "  beta", "zeta"
        ]
    );
}
