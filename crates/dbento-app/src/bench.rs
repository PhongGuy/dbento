//! The self-driving bench window behind the `bench` feature: the real shell at 1280 x 800,
//! driven one frame at a time by [`Driver`], writing a [`BenchReport`] for `xtask app-bench`.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dbento_bench::cases::{PgUrl, Targets};
use dbento_bench::measure::vm_hwm_kib;
use dbento_core::MemoryStore;
use gpui_kit::{App, AppContext as _, Bounds, Entity, Window, WindowBounds, px, size};

use crate::app::{AppPaths, Services, window_options};
use crate::bench_driver::{
    BenchConfig, BenchExit, BenchMode, BenchReport, CancelDriver, CancelStep, Driver, STDERR_LOG,
    Step, WARM_FRAMES, Warmup, end_label,
};
use crate::query_tab::QueryTab;
use crate::results_pane::shown;
use crate::shell::Shell;
use crate::tab_model::TabModel;
use crate::theme::{self, Appearance};

/// Runs the bench described by the JSON file at `config`. Exits 0 after writing the
/// report, 2 on any error.
pub fn main(config: PathBuf) -> ! {
    let _ = log::set_logger(&STDERR_LOG).map(|()| log::set_max_level(log::LevelFilter::Warn));
    let fail = |e: String| -> ! {
        eprintln!("dbento-app bench: {e}");
        std::process::exit(2)
    };
    let config: BenchConfig = std::fs::read(&config)
        .map_err(|e| e.to_string())
        .and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()))
        .unwrap_or_else(|e| fail(format!("cannot read {}: {e}", config.display())));
    let pg = match std::env::var("DBENTO_PG_17_STRESS_URL") {
        Ok(url) => Some(PgUrl::parse(&url).unwrap_or_else(|e| fail(e.to_string()))),
        Err(_) => None,
    };
    let password = pg.as_ref().map(|p| p.password.clone());
    let sqlite = std::env::var_os("DBENTO_STRESS_SQLITE").map(PathBuf::from);
    let targets = Targets::new(pg, sqlite).unwrap_or_else(|e| fail(e.to_string()));
    let state_dir = tempfile::tempdir().unwrap_or_else(|e| fail(e.to_string()));
    let services = Services {
        paths: AppPaths::in_dir(state_dir.path()),
        store: Arc::new(MemoryStore::new()),
        env: Arc::new(move |v| {
            (v == "DBENTO_BENCH_PG_PASSWORD")
                .then(|| password.clone())
                .flatten()
        }),
        secrets_in_memory: false,
    };
    let ws_dir = targets.dir().to_path_buf();
    // Stays Unreported unless the report is written or an error is printed, so a window
    // closed early still fails and says so.
    let code = Rc::new(Cell::new(BenchExit::Unreported));
    let exit = Rc::clone(&code);
    gpui_kit::application()
        .with_assets(crate::ui::AppAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            theme::install(cx);
            crate::tokio_bridge::init(cx);
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            if let Err(e) = open(config, services, ws_dir, Rc::clone(&code), cx) {
                eprintln!("dbento-app bench: {e}");
                code.set(BenchExit::Failed);
                cx.quit();
            }
        });
    drop(targets);
    drop(state_dir);
    let end = exit.get();
    if let Some(why) = end.message() {
        eprintln!("dbento-app bench: {why}");
    }
    std::process::exit(end.code())
}

// The loop's state, shared by each frame's re-armed callback.
struct Bench {
    config: BenchConfig,
    shell: Entity<Shell>,
    driver: Driver,
    report: BenchReport,
    run_start: Instant,
    cancel: CancelDriver,
    warmup: Warmup,
    code: Rc<Cell<BenchExit>>,
}

fn open(
    config: BenchConfig,
    services: Services,
    dir: PathBuf,
    code: Rc<Cell<BenchExit>>,
    cx: &mut App,
) -> Result<(), String> {
    let options = gpui_kit::WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(1280.0), px(800.0)),
            cx,
        ))),
        ..window_options()
    };
    let (handle, shell) = gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| {
            let saved = crate::persist::load_json(&services.paths.state).value;
            Shell::new(
                services,
                saved,
                Some(dir),
                Appearance::Dark,
                false,
                window,
                cx,
            )
        })
    })
    .map_err(|e| e.to_string())?;
    let bench = Rc::new(RefCell::new(Bench {
        driver: Driver::new(),
        report: BenchReport {
            case: config.case.clone(),
            ..BenchReport::default()
        },
        config,
        shell: shell.clone(),
        run_start: Instant::now(),
        cancel: CancelDriver::new(Instant::now()),
        warmup: Warmup::new(WARM_FRAMES),
        code,
    }));
    handle
        .update(cx, |_, window, cx| {
            let ix = shell.read(cx).workspace().and_then(|ws| {
                let name = &bench.borrow().config.connection;
                ws.connections().iter().position(|c| c.name() == name)
            });
            match ix {
                Some(ix) => shell.update(cx, |s, cx| s.open_connection(ix, window, cx)),
                None => {
                    return Err(format!(
                        "no connection {:?}: is its target variable set?",
                        bench.borrow().config.connection
                    ));
                }
            }
            let tab = tab(&shell, cx);
            let sql = bench.borrow().config.sql.clone();
            tab.update(cx, |t, cx| t.set_sql(&sql, window, cx));
            model(&tab, cx).update(cx, |m, _| m.set_max_rows(None));
            arm(window, Rc::clone(&bench));
            cx.activate(true);
            Ok(())
        })
        .map_err(|e| e.to_string())?
}

#[allow(clippy::expect_used)] // the bench opens its connection before driving any frame
fn tab(shell: &Entity<Shell>, cx: &App) -> Entity<QueryTab> {
    shell.read(cx).tab().expect("a connection is open").clone()
}

fn model(tab: &Entity<QueryTab>, cx: &App) -> Entity<TabModel> {
    tab.read(cx).model().clone()
}

fn hwm() -> Result<u64, String> {
    vm_hwm_kib().map_err(|e| e.to_string())
}

// Each frame re-arms the next one and refreshes, so frames keep coming at the display rate.
fn arm(window: &mut Window, bench: Rc<RefCell<Bench>>) {
    window.on_next_frame(move |window, cx| {
        let now = Instant::now();
        let done = {
            let mut b = bench.borrow_mut();
            match b.config.mode {
                _ if b.warmup.run_start().is_none() => warm_frame(&mut b, now, cx),
                BenchMode::Load => load_frame(&mut b, now, cx),
                BenchMode::Cancel => cancel_frame(&mut b, now, cx),
            }
        };
        match done {
            Ok(false) => {
                window.refresh();
                arm(window, bench);
            }
            Ok(true) => finish(&bench.borrow(), cx),
            Err(e) => {
                eprintln!("dbento-app bench: {e}");
                bench.borrow().code.set(BenchExit::Failed);
                cx.quit();
            }
        }
    });
}

// Presses Run once the window has drawn its warm frames, as a user would in a window
// already up; first paint and the cancel cap both count from here.
fn warm_frame(b: &mut Bench, now: Instant, cx: &mut App) -> Result<bool, String> {
    if b.warmup.on_frame(now) {
        b.report.hwm_before_kib = hwm()?;
        b.run_start = now;
        b.cancel = CancelDriver::new(now);
        tab(&b.shell, cx).update(cx, |t, cx| t.run_all(cx));
    }
    Ok(false)
}

fn load_frame(b: &mut Bench, now: Instant, cx: &mut App) -> Result<bool, String> {
    let tab = tab(&b.shell, cx);
    let model = model(&tab, cx);
    let m = model.read(cx);
    // The shown result is the grid's; without one, the last result (or the run's error) ends it.
    let result = shown(m.results())
        .or_else(|| m.results().len().checked_sub(1))
        .map(|i| &m.results()[i]);
    let rows = result.map_or(0, |r| r.store.rows());
    let ended = result.map_or(m.error().is_some(), |r| r.store.end().is_some());
    let end = match result.and_then(|r| r.store.end()) {
        Some(e) => end_label(e),
        None => m.error().unwrap_or("no result").to_owned(),
    };
    let heap = result.map_or(0, |r| r.store.heap_bytes());
    let has_more = m.has_more();
    if ended && b.report.hwm_loaded_kib == 0 {
        b.report.hwm_loaded_kib = hwm()?;
    }
    let step = b.driver.on_frame(now, rows as usize, has_more, ended);
    match step {
        Step::Pull => model.update(cx, |m, _| m.pull()),
        Step::Wait => {}
        Step::Finish => {
            b.report.rows = rows;
            b.report.heap_bytes = heap as u64;
            b.report.end = end;
            b.report.hwm_end_kib = hwm()?;
            b.report.first_paint_ms = b.driver.first_paint(b.run_start).map(ms);
            return Ok(true);
        }
    }
    Ok(false)
}

// Feeds the cancel driver from the first result; a cancel is timed only when it reached
// a statement still running.
fn cancel_frame(b: &mut Bench, now: Instant, cx: &mut App) -> Result<bool, String> {
    let tab = tab(&b.shell, cx);
    let model = model(&tab, cx);
    let m = model.read(cx);
    let executing = !m.results().is_empty();
    let end = m
        .results()
        .first()
        .and_then(|r| r.store.end())
        .map(end_label);
    let error = m.error().map(str::to_owned);
    let ended = end.is_some() || (!executing && error.is_some());
    match b.cancel.on_frame(now, executing, ended) {
        CancelStep::Wait => Ok(false),
        CancelStep::Cancel => {
            tab.update(cx, |t, cx| t.cancel(cx));
            Ok(false)
        }
        CancelStep::Finish(cancel) => {
            b.report.cancel_ms = cancel.map(ms);
            b.report.end = match (end, error) {
                (Some(end), _) => end,
                (None, Some(e)) => e,
                (None, None) if executing => "no end after cancel".into(),
                (None, None) => "never started".into(),
            };
            b.report.hwm_end_kib = hwm()?;
            Ok(true)
        }
    }
}

fn ms(d: Duration) -> f64 {
    d.as_micros() as f64 / 1000.0
}

fn finish(b: &Bench, cx: &mut App) {
    match write_report(&b.config.out, &b.report) {
        Ok(()) => b.code.set(BenchExit::Reported),
        Err(e) => {
            eprintln!("dbento-app bench: cannot write the report: {e}");
            b.code.set(BenchExit::Failed);
        }
    }
    cx.quit();
}

fn write_report(out: &Path, report: &BenchReport) -> Result<(), String> {
    let json = serde_json::to_vec_pretty(report).map_err(|e| e.to_string())?;
    std::fs::write(out, json).map_err(|e| e.to_string())
}
