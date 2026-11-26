# Contributing to Dbento

This is the developer guide; the page for people who use Dbento is [README.md](README.md).

## Sending a change

Dbento is open source under MIT or Apache-2.0, and issues and pull requests from anyone are welcome. A first pull request is fine too, and so is a small one: a typo, a clearer error message, a missing test.

Before I merge, I check that:

- `make check` passes: formatting, clippy with warnings as errors, and the tests (see [Checks and tests](#checks-and-tests)). CI runs it on every pull request, so you can lean on CI if you can't run it all locally.
- the pull request makes one change, ideally for one issue.

If something is missing, I'll say what and help get the pull request over the line. If you're not sure an idea fits, open an issue first and I'll answer there. The conventions are in [Sending a pull request](#sending-a-pull-request).

## Requirements

- Rust 1.99 or newer (the workspace's `rust-version` in `Cargo.toml`), from [rustup](https://rustup.rs).
- A C and C++ compiler and libclang, for the bundled `pg_query` parser and DuckDB.
- On Linux, the system libraries GPUI draws with. On Debian or Ubuntu, the same list CI installs:

  ```sh
  sudo apt-get install pkg-config libfontconfig-dev libfreetype-dev libwayland-dev \
    libx11-xcb-dev libxkbcommon-x11-dev libvulkan1 libegl1 libclang-dev
  ```

Some checks need extra tools, named with each target under [Checks and tests](#checks-and-tests).

## Build from source

From a checkout, run:

    make

This builds with 4 jobs (`make JOBS=8` for more) and installs `dbento` (the command line) and `dbento-app` (the desktop app) with `cargo install`. On Linux it also adds a desktop entry and the icon under `~/.local/share`. `make PREFIX=/some/dir` installs the binaries under `/some/dir/bin` instead, and `make uninstall` removes everything again. On macOS only the binaries are installed. On Windows, or to install without `make`, run the two installs by hand:

    cargo install --locked --path crates/dbento-cli
    cargo install --locked --path crates/dbento-app

To build without installing, `cargo build --release` puts both binaries in `target/release`.

DuckDB, MongoDB, SurrealDB, Redis and DynamoDB are default Cargo features (`duckdb`, `mongodb`, `surrealdb`, `redis`, `dynamodb`) of the core, the command line, the MCP server and the app. A build with `--no-default-features` leaves them out, and leaving out DuckDB skips its C++ compile.

## Try the demo

    make demo

This opens the app on a small shop: customers, products, orders and 200,000 events in SQLite, with saved queries to open with Ctrl+O (Cmd+O on macOS) from the workspace's `queries/` folder (top customers, a scroll through every event, an `UPDATE` without `WHERE`, a slow query to cancel, and a write that the read-only `shop-prod` connection refuses). No database server or keychain is needed. Each run copies `examples/demo` fresh to `~/.cache/dbento-demo/workspace` and builds `shop.sqlite` from its `seed.sql`, and the app keeps its state in `~/.cache/dbento-demo/state`, so nothing you do in the demo reaches the repo or your own app settings.

## Layout

All of the logic lives in `dbento-core`, a library with no UI: the workspace format, the keychain, the database drivers behind one async interface, streamed results, and the safety checks. The shells call only its public API.

| Path | What it is |
| --- | --- |
| `crates/dbento-core` | The UI-free core. |
| `crates/dbento-workspace` | The `dbento.toml` format: model, parser, validator and the `queries/` layout, described in [FORMAT.md](crates/dbento-workspace/FORMAT.md). |
| `crates/dbento-cli` | The `dbento` command line, which also hosts the MCP server. |
| `crates/dbento-mcp` | The MCP server itself, built on rmcp. |
| `crates/dbento-app` | The `dbento-app` desktop app, built on GPUI. |
| `crates/dbento-rules` | Dbento's push rules for a Potluck team server. |
| `crates/dbento-testkit` | Test-only database containers and fresh databases. |
| `crates/dbento-bench` | Release-build measurements of the performance targets. |
| `crates/xtask` | Repository automation, run as `cargo xtask`. |
| `tools/dbento-pilot` | The gpui-pilot driver for the app, a separate workspace with its own lock. |
| `examples/demo` | The demo workspace that `make demo` copies. |
| `scripts/` | The install, demo, packaging, release and QA scripts, with their tests in `scripts/tests`. |
| `vendor/` | Patched upstream crates. |

`dbento-workspace` and `dbento-rules` must stay free of database drivers, tokio and `toml_edit` on every target; `make check` enforces it.

## Checks and tests

`make check` is the merge gate: a RustSec advisory scan with `cargo deny` (it needs the network), `cargo fmt --check`, clippy with warnings as errors, the full test suite, and the dependency check on `dbento-workspace` and `dbento-rules`. `make fmt` formats the workspace. Before you open a pull request, run `make check`, and `make pilot-check` for a change to the app.

| Target | What it does |
| --- | --- |
| `make check` | The merge gate, as above. |
| `make pilot-check` | Builds and runs the gpui-pilot tests in `tools/dbento-pilot`, which drive the real app, except the two wheel-scroll timing recordings. |
| `make pilot-timing` | The wheel-scroll timing recordings alone. They wait for a quiet machine and refuse to measure a loaded one; a pass stamps the committed tree it ran on. |
| `make timing-guard` | Fails when the grid, its scrolling or rendering, the wheel test or the gpui pins changed since `BASE` (default `origin/master`) and the current tree has no `pilot-timing` stamp. |
| `make each-feature` | Checks each engine feature of the core, the command line and the MCP server on its own, with `cargo hack`. |
| `make script-test` | The bash suites under `scripts/tests`. |
| `make workflows-lint` | actionlint over `.github/workflows`. |
| `make icons-check`, `make notices-check`, `make windows-check` | The icon exports, the third-party notices (needs `cargo-about`), and a Windows type-check through mingw. |
| `make xwin-check`, `make xwin-build` | An msvc check and release-style build from Linux through cargo-xwin; `xwin-build` compiles shaders at runtime, so it is not for shipping. |
| `make package` | The Linux CLI archive, `.deb` and AppImage, verified, in `target/dist/linux`. |
| `make screenshots` | Regenerates the README screenshots in `assets/screenshots` from the demo workspace under gpui-pilot. |
| `make demo` | Opens the app on the demo workspace. |

A change to the results grid, its scrolling or rendering merges only once `make timing-guard` passes: run `make pilot-timing` on a quiet machine on the commit you merge. The paths it watches are listed in `scripts/timing-guard.sh`.

The full suite builds DuckDB and starts many containers, and it can take a lot of memory. On Linux I run it with fewer build jobs, without debug info, inside a memory cap, so a test that runs away is stopped before it takes the machine down:

```sh
CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 \
  systemd-run --user --scope -p MemoryMax=12G -p MemorySwapMax=0 make check
```

DuckDB's bundled C++ takes about eight minutes in every fresh target dir. With [sccache](https://github.com/mozilla/sccache) on `PATH` (for example in `~/.cargo/bin` after `cargo install --locked sccache`), I run `eval "$(scripts/sccache-env.sh)"` before cargo or make to cache it: the DuckDB step then takes about half a minute, and other worktrees share the cache. Keep it on or off for a given target dir, since switching reruns the C++ build. A `CC_<target>` or `CXX_<target>` variable overrides `CC` and `CXX` in the cc crate and bypasses the cache. Without sccache the script prints nothing.

### Tests against real databases

Tests against database servers start containers through testcontainers: Postgres, MySQL and MariaDB, SQL Server, MongoDB, Valkey for Redis, SurrealDB, and Moto for DynamoDB. With rootless podman, point it at the podman socket:

    export DOCKER_HOST=unix://$XDG_RUNTIME_DIR/podman/podman.sock

`cargo xtask db up` starts a matrix of Postgres versions with the Chinook dataset (`--datasets` adds `pagila`, `airlines`, `airlines-medium`, `airlines-big` or the `stress` tables), or with `--engine mysql`, `mariadb` or `mssql` those servers, and prints the `export` lines that point the tests at it; `cargo xtask db down` removes the containers. To skip containers, point a variable at a running server: `DBENTO_PG_URL`, `DBENTO_MYSQL_URL`, `DBENTO_MARIADB_URL`, `DBENTO_MSSQL_URL`, `DBENTO_MONGO_URL`, `DBENTO_REDIS_URL`, `DBENTO_SURREAL_URL` or `DBENTO_DYNAMO_URL`.

The SQL Server container starts only once `DBENTO_MSSQL_ACCEPT_EULA=Y` accepts Microsoft's licence. Without that variable and without `DBENTO_MSSQL_URL`, the SQL Server tests fail and say why, unless `CI` is set: then they are skipped. To run the suite locally without accepting the licence, run it with `CI=1`. SQL Server joins the shared servers that `make test` starts for the run only when `DBENTO_MSSQL_ACCEPT_EULA=Y` is set: CI sets it, and a contributor who accepts the licence may set it too.

## CI

Every push to `master` and every pull request runs `.github/workflows/ci.yml`:

- **gate:** `make workflows-lint script-test check icons-check windows-check notices-check` with `LOCKED=--locked`, so a stale `Cargo.lock` fails. SQL Server runs here, since the licence is accepted for this job.
- **macos-clippy:** clippy on macOS.
- **each-feature:** `make each-feature`.
- **pilot:** `make pilot-check` against gpui-pilot on software Vulkan, without the session-mode scenarios, which need KDE's KWin and stay in the local gate along with the wheel recordings.

A run dispatched with `windows` set also builds on Windows and runs the updater tests there. `zizmor.yml` audits the workflows on every push to `master` and every pull request, and `attribution.yml` fails a pull request whose title, body or commits credit an AI agent. On a schedule, `advisories.yml` scans both lockfiles daily and `weekly.yml` runs clippy on beta and a check after `cargo update`.

## Filing an issue

Open an issue with one of the templates: bug report, feature request, looks wrong or feels clunky, performance, chore, or documentation. Each one sets its label. For a bug, give the version from `dbento --version` or the About dialog, and remove anything secret from logs and SQL before pasting.

## Sending a pull request

- One pull request per issue, with `Closes #N` in its body.
- The pull request carries its issue's label (`feature`, `bug`, `ux`, `accessibility`, `perf`, `docs` or `chore`); the release notes group merged pull requests by label through `.github/release.yml`, and `chore`, `ci`, `dependencies` and `skip-changelog` are left out of them.
- Give it a title in plain words, not a Conventional Commits prefix.
- Write commit messages in the [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/) style, such as `fix(core): ...`, with the types `feat`, `fix`, `chore`, `refactor`, `docs` and `test`.
- Do not credit an AI agent in a commit or the pull request: no AI `Co-authored-by:` line and no "Generated with" footer. You take the credit and the responsibility for what you send. The attribution check fails a pull request that does.
- Run `make check` first, and `make pilot-check` for a change to the app.

## Pilot QA

The pilot crate (`tools/dbento-pilot`) runs the app under [gpui-pilot](https://github.com/PhongGuy/gpui-pilot) v0.9.3. `make pilot-check` and `make screenshots` need the `gpui-pilot` server on your `PATH`, or `GPUI_PILOT_BIN` naming it. Install it with:

```sh
cargo install --locked --git https://github.com/PhongGuy/gpui-pilot --tag v0.9.3 gpui-pilot
```

`gpui-pilot.toml` tells the pilot how to build and launch Dbento. Headless mode runs `tools/dbento-pilot` against a fresh data directory, with an optional workspace folder as the launch argument; `DBENTO_PILOT_THEME` (`dark`, `light` or `system`, through the launch's `env`) seeds the theme, and `light` is the only way to a light shot, since the headless host reports a dark system. Session mode runs the optimised `dbento-app` from the `timing` profile in a private KDE KWin session.

For hand-run QA, `scripts/qa-workspace.sh <dir>` builds a workspace with the Chinook SQLite database and four connections, and `scripts/pilot-qa.sh <name>` reads one pilot call per line (`tool {json}`) from stdin, with `$WS` replaced by a fresh QA workspace, and drives the app through them with gpui-pilot's `scripts/drive.py` (`DRIVE_PY`, default `~/git/gpui-pilot/scripts/drive.py`). It needs Python 3 and keeps the screenshots and element trees it takes under `~/.cache`.

## Releases

How a release is cut and checked is in [RELEASING.md](RELEASING.md).
