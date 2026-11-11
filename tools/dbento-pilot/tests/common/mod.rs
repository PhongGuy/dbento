//! The pilot harness shared by the MCP tests: a `gpui-pilot` server in a scratch project,
//! plus helpers that open a SQLite workspace, run a query and read the element tree.
#![allow(dead_code)]

use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::TokioChildProcess;
use rmcp::{RoleClient, ServiceExt};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::process::Command;

/// The server binary: `GPUI_PILOT_BIN`, else `gpui-pilot` on `PATH`.
pub fn server_bin() -> PathBuf {
    if let Some(bin) = std::env::var_os("GPUI_PILOT_BIN") {
        return PathBuf::from(bin);
    }
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join("gpui-pilot"))
        .find(|bin| bin.is_file())
        .expect("no gpui-pilot server: install it (see gpui-pilot.toml) or set GPUI_PILOT_BIN")
}

/// The optimised app the session backend launches (gpui-pilot.toml's session build). bwrap
/// binds it at its own path, so an existing binary is canonicalised to drop any `..`.
pub fn session_app() -> PathBuf {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"));
    let app = target.join("timing/dbento-app");
    std::fs::canonicalize(&app).unwrap_or(app)
}

pub struct Pilot {
    pub client: RunningService<RoleClient, ()>,
    _root: TempDir,
    _temp: TempDir,
    /// Stand-in session bus; nothing under the pilot may connect to it.
    bus: UnixListener,
}

impl Pilot {
    /// Starts the server in a scratch project whose config launches this package's binary
    /// headless and the optimised app in a session, with a cleared environment that points
    /// the session bus and displays at canaries.
    pub async fn start() -> Pilot {
        let (root, temp) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let config = format!(
            "[headless]\nbinary = {:?}\n\n[session]\nbinary = {:?}\n",
            env!("CARGO_BIN_EXE_dbento-pilot"),
            session_app()
        );
        std::fs::write(root.path().join("gpui-pilot.toml"), config).unwrap();
        let bus_path = temp.path().join("session_bus");
        let bus = UnixListener::bind(&bus_path).unwrap();
        bus.set_nonblocking(true).unwrap();
        let mut command = Command::new(server_bin());
        command
            .arg("mcp")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", root.path())
            .env("TMPDIR", temp.path())
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={}", bus_path.display()),
            )
            .env("DISPLAY", ":99")
            .env("WAYLAND_DISPLAY", "gpui-pilot-canary")
            .current_dir(root.path());
        let client = ().serve(TokioChildProcess::new(command).unwrap()).await.unwrap();
        Pilot {
            client,
            _root: root,
            _temp: temp,
            bus,
        }
    }

    /// Calls `tool`, requires success, and returns the whole result.
    pub async fn call(&self, tool: &str, args: Value) -> CallToolResult {
        let Value::Object(args) = args else {
            panic!("arguments must be an object")
        };
        let result = self
            .client
            .call_tool(CallToolRequestParams::new(tool.to_owned()).with_arguments(args))
            .await
            .unwrap_or_else(|e| panic!("{tool}: {e}"));
        assert_ne!(
            result.is_error,
            Some(true),
            "{tool} failed: {:?}",
            result.structured_content
        );
        result
    }

    pub async fn ok(&self, tool: &str, args: Value) -> Value {
        self.call(tool, args)
            .await
            .structured_content
            .expect("structured content")
    }

    pub fn assert_bus_untouched(&self) {
        match self.bus.accept() {
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            other => panic!("something connected to the session bus: {other:?}"),
        }
    }
}

/// A launch in `mode` at 1280 x 800, scale 1, on workspace `ws`, without a build.
pub fn launch_args(mode: &str, ws: &Path) -> Value {
    json!({ "mode": mode, "build": false, "width": 1280, "height": 800, "scale": 1.0, "args": [ws] })
}

/// A workspace whose one connection, `lite`, is an empty SQLite file.
pub fn sqlite_workspace() -> TempDir {
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(
        ws.path().join("dbento.toml"),
        "dbento = 1\n\n[connections.lite]\nengine = \"sqlite\"\npath = \"app.db\"\n",
    )
    .unwrap();
    std::fs::write(ws.path().join("app.db"), b"").unwrap();
    ws
}

#[derive(Clone, Copy, Debug)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// The tree node for element `id`, if any: a session node carries it as `id`, a headless
/// node as the last segment of its `path`.
pub fn find_node<'a>(tree: &'a Value, id: &str) -> Option<&'a Value> {
    tree["nodes"]
        .as_array()
        .expect("tree nodes")
        .iter()
        .find(|n| {
            n["id"] == json!(id) || n["path"].as_array().and_then(|p| p.last()) == Some(&json!(id))
        })
}

pub fn node<'a>(tree: &'a Value, id: &str) -> &'a Value {
    find_node(tree, id).unwrap_or_else(|| panic!("no {id} in the tree: {tree}"))
}

pub fn bounds(tree: &Value, id: &str) -> Rect {
    let b = &node(tree, id)["bounds"];
    let at = |k: &str| {
        b[k].as_f64()
            .unwrap_or_else(|| panic!("{id} has no bounds: {b}"))
    };
    Rect {
        x: at("x"),
        y: at("y"),
        w: at("w"),
        h: at("h"),
    }
}

/// The row count the result footer shows ("2,048 rows", "1,000+ rows").
pub fn rows_loaded(tree: &Value) -> u64 {
    let name = node(tree, "footer-rows")["name"]
        .as_str()
        .expect("footer-rows has a name");
    footer_count(name).unwrap_or_else(|| panic!("no count in {name:?}"))
}

fn footer_count(name: &str) -> Option<u64> {
    name.chars()
        .take_while(|c| c.is_ascii_digit() || *c == ',')
        .filter(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()
}

/// Opens connection `lite` with a double click, types `sql`, lifts the row limit when
/// `unlimited`, runs it and waits for the footer to count a first row.
pub async fn run_query(pilot: &Pilot, sql: &str, unlimited: bool) {
    pilot
        .ok(
            "click",
            json!({ "target": { "id": ["connection-lite"] }, "count": 2 }),
        )
        .await;
    pilot
        .ok("tree", json!({ "wait_for": { "id": ["editor"] } }))
        .await;
    pilot
        .ok(
            "type",
            json!({ "target": { "id": ["editor"] }, "text": sql }),
        )
        .await;
    if unlimited {
        // The row-limit button cycles 1k, 10k, 100k, 1M, then no limit.
        for _ in 0..4 {
            pilot
                .ok("click", json!({ "target": { "id": ["row-limit"] } }))
                .await;
        }
        let tree = pilot.ok("tree", json!({})).await;
        assert_eq!(node(&tree, "row-limit")["name"], json!("no limit"));
    }
    pilot
        .ok("click", json!({ "target": { "id": ["run-all"] } }))
        .await;
    // Grid cells carry no AT-SPI id, so both modes wait on the footer, which renders in the
    // same frame as the rows it counts.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let tree = pilot.ok("tree", json!({})).await;
        let counted = find_node(&tree, "footer-rows")
            .and_then(|n| n["name"].as_str())
            .and_then(footer_count);
        if counted.is_some_and(|n| n > 0) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no rows within 30 s: {tree}"
        );
        pilot.ok("wait", json!({ "ms": 50 })).await;
    }
}

/// The committed demo workspace, `make demo`'s source.
const DEMO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/demo");

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// A fresh copy of the demo workspace in a folder named `shop` (the name the approval dialog
/// shows), with shop.sqlite built from its seed through core, as `make demo` builds it.
pub async fn demo_workspace() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("shop");
    copy_dir(Path::new(DEMO), &ws);
    std::fs::write(ws.join("shop.sqlite"), b"").unwrap();
    let seed = std::fs::read_to_string(ws.join("seed.sql")).unwrap();
    let w = dbento_core::Workspace::open(&ws).unwrap();
    let conn = w.declared("shop").unwrap();
    let mut s = dbento_core::connect(&w, conn, None, Default::default())
        .await
        .unwrap();
    {
        let mut script = s.script(&seed);
        while let Some(p) = script.next().await {
            let mut stream = p.unwrap().execute(Default::default()).await.unwrap();
            while stream.next_batch().await.unwrap().is_some() {}
        }
    }
    s.close().await.unwrap();
    (dir, ws)
}
