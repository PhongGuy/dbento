//! The quit that installs an armed release and relaunches, and the relaunch command.
use crate::support;
use crate::update_arm::{IMAGE, Rig, armed, prepare, rig_with};
use dbento_app::updates::{self, Launcher};
use dbento_core::update::{Os, Relaunch};
use gpui_kit::TestAppContext;
use std::cell::{Cell, RefCell};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::{Child, Command, ExitStatus, Stdio};
use std::rc::Rc;
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

// An executable script standing in for the new install; running it leaves `ran` behind.
#[cfg(unix)]
fn marker(dir: &Path) -> (PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt as _;
    let ran = dir.join("ran");
    let script = dir.join("Dbento.AppImage");
    std::fs::write(&script, format!("#!/bin/sh\n: > '{}'\n", ran.display())).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    (script, ran)
}

// Waits up to ten seconds for `child` to exit; one that never does is killed and reaped.
#[cfg(unix)]
fn exit_within(child: &mut Child) -> Option<ExitStatus> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            return Some(status);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    None
}

#[derive(Default)]
struct Recorder {
    calls: RefCell<Vec<String>>,
}
impl Launcher for Recorder {
    fn restart(&self, path: &Path) -> std::io::Result<()> {
        self.calls
            .borrow_mut()
            .push(format!("restart {}", path.display()));
        Ok(())
    }
    fn start_installer(&self, program: &Path, args: &[OsString]) -> std::io::Result<()> {
        self.calls
            .borrow_mut()
            .push(format!("installer {} {args:?}", program.display()));
        Ok(())
    }
}

fn rig(cx: &mut TestAppContext, then: Relaunch) -> (Rig, Rc<Recorder>) {
    let launched = Rc::new(Recorder::default());
    let l = Rc::clone(&launched);
    let r = rig_with(cx, then, None, false, move |h| {
        h.with_launcher(l as Rc<dyn Launcher>)
    });
    (r, launched)
}
fn calls(l: &Recorder) -> Vec<String> {
    l.calls.borrow().clone()
}

#[gpui_kit::test]
fn quitting_installs_only_an_armed_release(cx: &mut TestAppContext) {
    let (r, launched) = rig(cx, Relaunch::Restart(IMAGE.into()));
    prepare(cx, &r);
    cx.update(updates::quit_install);
    assert!(
        calls(&launched).is_empty(),
        "an unarmed release was installed"
    );
    r.updates.update(cx, |u, cx| u.arm(cx));
    cx.update(updates::quit_install);
    assert_eq!(calls(&launched), [format!("restart {IMAGE}")]);
    assert!(!armed(cx, &r));
}

#[gpui_kit::test]
fn the_app_quit_hook_starts_the_installer(cx: &mut TestAppContext) {
    let program = PathBuf::from("/tmp/dbento-update-99.0.0-setup.exe");
    let args = vec![OsString::from("/P"), OsString::from("/R")];
    let (r, launched) = rig(
        cx,
        Relaunch::Installer {
            program: program.clone(),
            args: args.clone(),
        },
    );
    prepare(cx, &r);
    r.updates.update(cx, |u, cx| u.arm(cx));
    cx.quit();
    assert_eq!(
        calls(&launched),
        [format!("installer {} {args:?}", program.display())]
    );
}

// GPUI runs every quit observer's body before awaiting any returned future, in registration
// order, so an observer registered after `install` sees the launch only if it ran in the body.
#[gpui_kit::test]
fn the_install_runs_in_the_quit_hook_body_not_its_future(cx: &mut TestAppContext) {
    let (r, launched) = rig(cx, Relaunch::Restart(IMAGE.into()));
    prepare(cx, &r);
    r.updates.update(cx, |u, cx| u.arm(cx));
    let seen = Rc::new(Cell::new(usize::MAX));
    let (sink, watched) = (Rc::clone(&seen), Rc::clone(&launched));
    let _later = cx.update(|cx| {
        cx.on_app_quit(move |_| {
            sink.set(watched.calls.borrow().len());
            async {}
        })
    });
    cx.quit();
    assert_eq!(
        seen.get(),
        1,
        "the install ran in the returned future, past the 200 ms cap"
    );
}

#[gpui_kit::test]
fn download_and_restart_arms_then_closes_the_window(cx: &mut TestAppContext) {
    let (r, launched) = rig(cx, Relaunch::Restart(IMAGE.into()));
    let files = tempfile::tempdir().unwrap();
    let (win, shell) = cx
        .update(|cx| dbento_app::app::start(support::services(files.path()), None, cx))
        .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        win.update(cx, |_, window, cx| {
            updates::download_and_restart(&shell, window, cx)
        })
    })
    .unwrap();
    cx.run_until_parked();
    assert!(
        cx.update(|cx| cx.windows().is_empty()),
        "the window stayed open"
    );
    assert!(armed(cx, &r));
    cx.quit();
    assert_eq!(calls(&launched), [format!("restart {IMAGE}")]);
}

#[test]
fn the_relaunch_waits_for_this_process_then_starts_the_new_install() {
    let args = |c: &std::process::Command| -> Vec<String> {
        c.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    };
    let linux =
        updates::relaunch_command(Path::new(IMAGE), 4242, Os::Linux, Duration::from_secs(30))
            .unwrap();
    assert_eq!(linux.get_program(), "/bin/sh");
    let a = args(&linux);
    assert_eq!(a[0], "-c");
    assert!(
        a[1].contains("kill -0") && a[1].contains("exec \"$1\"") && a[1].contains("\"$2\""),
        "{}",
        a[1]
    );
    assert!(
        !a[1].contains("4242") && !a[1].contains(IMAGE),
        "the script interpolates its arguments: {}",
        a[1]
    );
    assert_eq!(a[2..], ["4242", IMAGE, "300"]);
    let mac = updates::relaunch_command(
        Path::new("/Applications/Dbento.app"),
        7,
        Os::Macos,
        Duration::from_millis(250),
    )
    .unwrap();
    let m = args(&mac);
    assert!(m[1].contains("exec /usr/bin/open \"$1\""), "{}", m[1]);
    assert_eq!(m[2..], ["7", "/Applications/Dbento.app", "3"]);
    assert!(
        updates::relaunch_command(Path::new("C:/x"), 7, Os::Windows, Duration::from_secs(30))
            .is_none(),
        "Windows relaunches through its installer"
    );
}

// A process that outlives the wait is left alone: the shell says so and never starts the install.
#[cfg(unix)]
#[test]
fn the_relaunch_gives_up_on_a_process_that_outlives_the_wait() {
    use std::io::Read as _;
    let dir = tempfile::tempdir().unwrap();
    let (target, ran) = marker(dir.path());
    let mut hung = Command::new("sleep").arg("30").spawn().unwrap();
    let mut shell =
        updates::relaunch_command(&target, hung.id(), Os::Linux, Duration::from_millis(300))
            .unwrap();
    let mut child = shell
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let status = exit_within(&mut child);
    let _ = hung.kill();
    let _ = hung.wait();
    let mut err = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut err)
        .unwrap();
    assert_eq!(status.and_then(|s| s.code()), Some(1), "stderr: {err}");
    assert!(err.contains("gave up"), "stderr: {err}");
    assert!(
        !ran.exists(),
        "the relaunch started the install beside a live process"
    );
}

#[cfg(unix)]
#[test]
fn the_relaunch_starts_the_install_once_the_process_is_gone() {
    let dir = tempfile::tempdir().unwrap();
    let (target, ran) = marker(dir.path());
    let mut gone = Command::new("true").spawn().unwrap();
    let pid = gone.id();
    gone.wait().unwrap();
    let mut shell =
        updates::relaunch_command(&target, pid, Os::Linux, Duration::from_secs(5)).unwrap();
    let mut child = shell
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let status = exit_within(&mut child);
    assert!(status.is_some_and(|s| s.success()), "{status:?}");
    assert!(ran.exists(), "the new install never started");
}
