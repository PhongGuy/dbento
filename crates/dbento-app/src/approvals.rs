//! The approvals dialog: one MCP write at a time, shown in full, answered only by its buttons.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use dbento_core::{
    ApprovalRequest, ApprovalStore, Color, Decision, Unguarded, Verdict, Workspace, Writes,
    leading_keyword,
};
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme as _, Placement, WindowExt, h_flex, v_flex};
use gpui_kit::{
    App, AppContext as _, FocusHandle, InteractiveElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, TestSupportExt, Window, div, px,
};

use crate::confirm::{DIALOG_WIDTH, dialog_top, footer, frame, title_row};
use crate::sidebar::tag_color;
use crate::ui::{Notice, NoticeKind};
use crate::{theme, ui};

/// How often the app looks for new approval requests.
pub const APPROVAL_POLL: Duration = Duration::from_secs(1);

/// One request as the dialog shows it, with its connection's tag read from its own workspace.
#[derive(Debug, Clone)]
pub struct Shown {
    /// The request as filed.
    pub request: ApprovalRequest,
    /// The connection's effective tag, if its workspace still names it.
    pub color: Option<Color>,
    /// Whether the connection is tagged production.
    pub production: bool,
    /// How many requests were pending when it was read, itself included.
    pub queued: usize,
}

/// The oldest pending request under `dir` (pruning as `pending()` does), or None when there
/// is none or the folder cannot be opened. Blocking: call it off the main thread.
pub fn next_pending(dir: &Path) -> Option<Shown> {
    let pending = ApprovalStore::open(dir).ok()?.pending();
    let queued = pending.len();
    let request = pending.into_iter().next()?;
    // The tag comes from the request's own workspace; any failure shows it untagged.
    let tag = Workspace::open(request.workspace()).ok().and_then(|ws| {
        ws.connection(request.connection())
            .map(|c| (c.effective_color(), c.is_production()))
    });
    let (color, production) = tag.unwrap_or((None, false));
    Some(Shown {
        request,
        color,
        production,
        queued,
    })
}

/// Pending requests under `dir` (pruning as `pending()` does); empty when the folder cannot
/// be opened. Blocking: call it off the main thread.
pub fn pending_requests(dir: &Path) -> Vec<ApprovalRequest> {
    ApprovalStore::open(dir).map_or_else(|_| Vec::new(), |store| store.pending())
}

/// How many requests under `dir` are pending, or 0 when the folder cannot be opened.
/// Blocking: call it off the main thread.
pub fn pending_count(dir: &Path) -> usize {
    pending_requests(dir).len()
}

/// Which `dbento` the connect command names.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliPath {
    /// `dbento`, found on the PATH.
    Bare,
    /// The app runs from an AppImage: bare `dbento`, plus a hint to install the CLI archive.
    AppImage,
    /// The CLI bundled beside the app.
    Absolute(PathBuf),
}

impl CliPath {
    /// AppImage when `env("APPIMAGE")` is non-empty; else Absolute when `exe` is absolute and
    /// a regular file `dbento` + EXE_SUFFIX sits beside it; else Bare.
    pub fn resolve(exe: Option<&Path>, env: &dyn Fn(&str) -> Option<String>) -> CliPath {
        // An AppImage's sibling lives in a mount that vanishes on exit, so it never names the CLI.
        if env("APPIMAGE").is_some_and(|v| !v.is_empty()) {
            return CliPath::AppImage;
        }
        let sibling = exe
            .filter(|e| e.is_absolute())
            .and_then(Path::parent)
            .map(|dir| dir.join(format!("dbento{}", std::env::consts::EXE_SUFFIX)))
            .filter(|cli| cli.is_file());
        sibling.map_or(CliPath::Bare, CliPath::Absolute)
    }

    /// `resolve` over `std::env::current_exe()` and the process environment.
    pub fn current() -> CliPath {
        let exe = std::env::current_exe().ok();
        CliPath::resolve(exe.as_deref(), &|k| std::env::var(k).ok())
    }
}

/// How a command line quotes its arguments.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quoting {
    /// Single quotes around any token with a byte outside `[A-Za-z0-9_./-]`.
    Posix,
    /// Double quotes around any token holding a space or tab.
    Windows,
}

impl Quoting {
    /// Windows on Windows, else Posix.
    pub fn native() -> Quoting {
        if cfg!(windows) {
            Quoting::Windows
        } else {
            Quoting::Posix
        }
    }

    fn quote(self, token: &str) -> String {
        match self {
            Quoting::Posix => {
                let plain = token
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_./-".contains(&b));
                if plain {
                    token.to_owned()
                } else {
                    format!("'{}'", token.replace('\'', "'\\''"))
                }
            }
            Quoting::Windows if token.contains([' ', '\t']) => format!("\"{token}\""),
            Quoting::Windows => token.to_owned(),
        }
    }
}

/// The hint shown under the command inside an AppImage.
pub const APPIMAGE_HINT: &str = "Install the dbento CLI archive to put dbento on your PATH.";

/// `<cli> --workspace <dir> mcp`, quoted for this OS.
pub fn connect_command(cli: &CliPath, dir: &Path) -> String {
    connect_command_with(cli, dir, Quoting::native())
}

/// `connect_command` with explicit quoting.
pub fn connect_command_with(cli: &CliPath, dir: &Path, quoting: Quoting) -> String {
    let program = match cli {
        CliPath::Absolute(path) => quoting.quote(&path.to_string_lossy()),
        _ => "dbento".to_owned(),
    };
    let dir = quoting.quote(&dir.to_string_lossy());
    format!("{program} --workspace {dir} mcp")
}

/// APPIMAGE_HINT for AppImage, else None.
pub fn cli_hint(cli: &CliPath) -> Option<&'static str> {
    matches!(cli, CliPath::AppImage).then_some(APPIMAGE_HINT)
}

/// The connection and what the statement does: for mongosh text opening with `db.` or `db[`,
/// its method, "shop · deleteMany"; otherwise the SQL's leading keyword in capitals,
/// "shop · UPDATE". The connection alone when neither is found.
pub fn headline(connection: &str, sql: &str) -> String {
    let what = match shell_target(sql) {
        Some(rest) => shell_method(rest).unwrap_or_default().to_owned(),
        None => leading_keyword(sql).to_ascii_uppercase(),
    };
    match what.as_str() {
        "" => connection.to_owned(),
        what => format!("{connection} · {what}"),
    }
}

// The text after a leading `db`, when the statement (past whitespace and comments) opens with
// `db.` or `db[`; None for anything else, SQL included.
fn shell_target(text: &str) -> Option<&str> {
    let mut rest = text;
    loop {
        rest = rest.trim_start();
        if let Some(after) = rest.strip_prefix("//") {
            rest = after.split_once('\n').map_or("", |(_, tail)| tail);
        } else if let Some(after) = rest.strip_prefix("/*") {
            rest = after.split_once("*/").map_or("", |(_, tail)| tail);
        } else {
            break;
        }
    }
    let rest = rest.strip_prefix("db")?;
    rest.starts_with(['.', '[']).then_some(rest)
}

// The first identifier called at bracket depth 0, skipping the getSiblingDB and getCollection
// hops; quoted strings are stepped over so a bracket inside one never counts.
fn shell_method(text: &str) -> Option<&str> {
    let mut depth = 0usize;
    let mut ident: Option<(usize, usize)> = None;
    let mut chars = text.char_indices();
    while let Some((i, c)) = chars.next() {
        if c.is_alphanumeric() || c == '_' || c == '$' {
            ident = Some((ident.map_or(i, |(start, _)| start), i + c.len_utf8()));
            continue;
        }
        let called = ident.take().map(|(start, end)| &text[start..end]);
        match c {
            '\'' | '"' => {
                while let Some((_, s)) = chars.next() {
                    match s {
                        '\\' => {
                            chars.next();
                        }
                        s if s == c => break,
                        _ => {}
                    }
                }
            }
            '(' => {
                if depth == 0
                    && let Some(name) = called
                    && name != "getSiblingDB"
                    && name != "getCollection"
                {
                    return Some(name);
                }
                depth += 1;
            }
            '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    None
}

/// None for a parsed write or read with nothing unguarded; otherwise "may write (not parsed)"
/// for an unparsed write and each unguarded change ("UPDATE without WHERE", "DROP", ...),
/// joined by " · ".
pub fn class_label(writes: Writes, unguarded: &[Unguarded]) -> Option<String> {
    // A classification this build does not know reads as unparsed, the cautious side.
    let unparsed = !matches!(writes, Writes::Yes | Writes::No);
    let unparsed = unparsed.then_some("may write (not parsed)");
    let parts: Vec<&str> = unparsed
        .into_iter()
        .chain(unguarded.iter().map(|u| u.label()))
        .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// Whether Approve is drawn red: the connection is production or the write is unguarded.
pub fn approve_is_danger(production: bool, unguarded: &[Unguarded]) -> bool {
    production || !unguarded.is_empty()
}

/// The notice shown once an Approve is recorded: the AI must retry the call to run it.
pub fn approved_notice() -> Notice {
    Notice {
        text: "Approved. Ask your AI to retry the same call.".to_owned(),
        kind: NoticeKind::Success,
    }
}

/// The notice for a request another window answered first. It names a side only when the
/// store recorded one, so it never says approved for a denial or the reverse.
pub fn answered_elsewhere_notice(verdict: Verdict) -> Notice {
    let (text, kind) = match verdict {
        Verdict::Approved => (
            "Already approved in another window. Ask your AI to retry the same call.",
            NoticeKind::Success,
        ),
        Verdict::Denied => ("Already denied in another window.", NoticeKind::Info),
        _ => ("Already answered in another window.", NoticeKind::Info),
    };
    Notice {
        text: text.to_owned(),
        kind,
    }
}

/// Whether `id` still waits for an answer under `dir`; None when the folder cannot be opened.
/// Blocking: call it off the main thread.
pub fn still_pending(dir: &Path, id: &str) -> Option<bool> {
    let store = ApprovalStore::open(dir).ok()?;
    Some(store.pending().iter().any(|r| r.id() == id))
}

/// The recorded answer to `request` under `dir`, Unknown when none can be read.
/// Blocking: call it off the main thread.
pub fn verdict_of(dir: &Path, request: &ApprovalRequest) -> Verdict {
    ApprovalStore::open(dir).map_or(Verdict::Unknown, |store| store.verdict(request))
}

/// The user's answer in the dialog: a decision and, for a denial, the reason typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    /// Approve or Deny.
    pub decision: Decision,
    /// The trimmed reason for a denial; None when blank or when approving.
    pub reason: Option<String>,
}

/// An open approval dialog: its answer, and a way to close it when another window answered.
pub struct Asked {
    /// Yields the user's answer once.
    pub answer: async_channel::Receiver<Answer>,
    focus: FocusHandle,
}

impl Asked {
    /// Closes the dialog while focus is inside it. Focus elsewhere means it has closed or
    /// another dialog sits on top, and closing then would take the wrong one.
    pub fn close(&self, window: &mut Window, cx: &mut App) {
        if self.focus.contains_focused(window, cx) {
            window.close_dialog(cx);
        }
    }
}

/// Opens the modal; `answer` yields the user's decision once.
pub fn ask_approval(shown: Shown, window: &mut Window, cx: &mut App) -> Asked {
    let (tx, rx) = async_channel::bounded(1);
    // The content's own focus keeps the keys inside the dialog, away from any button, so no
    // key can answer it; the dialog itself ignores Enter, Escape and the backdrop.
    let focus = cx.focus_handle();
    let asked = Asked {
        answer: rx,
        focus: focus.clone(),
    };
    let focused = Rc::new(Cell::new(false));
    // One input for the dialog's life: the builder runs every frame and must not reset it.
    let reason_input =
        cx.new(|cx| InputState::new(window, cx).placeholder("Reason for denying (optional)"));
    let r = shown.request;
    let sql: SharedString = r.sql().to_owned().into();
    let connection: SharedString = headline(r.connection(), r.sql()).into();
    let class: Option<SharedString> = class_label(r.writes(), r.unguarded()).map(Into::into);
    // The folder name reads at a glance; the full path stays in the tooltip.
    let path: SharedString = r.workspace().display().to_string().into();
    let workspace: SharedString = r
        .workspace()
        .file_name()
        .map_or(path.clone(), |n| n.to_string_lossy().into_owned().into());
    let queue: Option<SharedString> =
        (shown.queued > 1).then(|| format!("1 of {}", shown.queued).into());
    let danger = approve_is_danger(shown.production, r.unguarded());
    let (color, production) = (shown.color, shown.production);
    window.defer(cx, move |window, cx| {
        window.open_dialog(cx, move |dialog, window, cx| {
            if !focused.replace(true) {
                window.focus(&focus, cx);
            }
            // Only a denial carries the typed reason; an approval needs none.
            let answer = |id: &'static str, decision: Decision| {
                let (tx, input) = (tx.clone(), reason_input.clone());
                Button::new(id).h(px(32.)).on_click(move |_, window, cx| {
                    let typed = input.read(cx).value().trim().to_owned();
                    let reason = (decision == Decision::Deny && !typed.is_empty()).then_some(typed);
                    let _ = tx.try_send(Answer { decision, reason });
                    window.close_dialog(cx);
                })
            };
            let theme = cx.theme();
            let (warning, border, radius) = (theme.warning, theme::code_edge(cx), theme.radius);
            let (mono, mono_size) = (theme.mono_font_family.clone(), theme.mono_font_size);
            let (dim, transparent) = (theme::dim(cx), theme.transparent);
            // The folder button lays its label out like the plain text lines around it.
            let line_height = window.text_style().line_height;
            let text = |id: &'static str, label: SharedString| {
                div()
                    .id(id)
                    .aria_label(label.clone())
                    .test_support()
                    .child(label)
            };
            let chip = production.then(|| {
                h_flex().child(
                    ui::production_chip(cx)
                        .id("approval-production")
                        .aria_label("Production")
                        .test_support(),
                )
            });
            let dot = color.map(|c| {
                div()
                    .flex_none()
                    .size(px(8.))
                    .rounded_full()
                    .bg(tag_color(c))
            });
            // A warning edge and title mark an AI asking, not the user's own risky write.
            frame(dialog, DIALOG_WIDTH, Some(warning), cx)
                .margin_top(dialog_top(window))
                .keyboard(false)
                .overlay_closable(false)
                .on_ok(|_, _, _| false)
                .on_cancel(|_, _, _| false)
                .child(
                    v_flex()
                        .id("approval-dialog")
                        .test_support()
                        .track_focus(&focus)
                        .child(title_row(
                            Some((ui::Icons::Bot, warning)),
                            "Approve an AI write?",
                            cx,
                        ))
                        .child(
                            v_flex()
                                .gap_2()
                                .px_4()
                                .pt_2()
                                .pb_4()
                                .text_sm()
                                .text_color(dim)
                                .children(chip)
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .children(dot)
                                        .child(text("approval-connection", connection.clone())),
                                )
                                .children(class.clone().map(|c| text("approval-class", c)))
                                // A plain-looking kit button, for the one tooltip that can sit
                                // beside its line: below it the path would cover the SQL.
                                .child(
                                    h_flex().child(
                                        Button::new("approval-workspace")
                                            .custom(
                                                ButtonCustomVariant::new(cx)
                                                    .color(transparent)
                                                    .foreground(dim)
                                                    .hover(transparent)
                                                    .active(transparent),
                                            )
                                            .label(workspace.clone())
                                            .accessibility_label(workspace.clone())
                                            .tab_stop(false)
                                            .h_auto()
                                            .p_0()
                                            .text_sm()
                                            .line_height(line_height)
                                            .tooltip(path.clone())
                                            .tooltip_placement(Placement::Right),
                                    ),
                                )
                                .child(
                                    text("approval-sql", sql.clone())
                                        .mt_1()
                                        .p_2()
                                        .rounded(radius)
                                        .border_1()
                                        .border_color(border)
                                        .bg(theme::editor_fill(cx))
                                        .text_color(cx.theme().foreground)
                                        .font_family(mono)
                                        .text_size(mono_size),
                                )
                                // Enter here reaches the dialog's on_ok, which answers nothing.
                                .child(
                                    div()
                                        .id("approval-reason")
                                        .test_support()
                                        .mt_1()
                                        .child(Input::new(&reason_input)),
                                ),
                        )
                        .child(
                            footer(cx)
                                .children(queue.clone().map(|q| text("approval-queue", q)))
                                .child(div().flex_1())
                                .child(
                                    ui::secondary(answer("approval-deny", Decision::Deny))
                                        .label("Deny"),
                                )
                                .child({
                                    let approve = answer("approval-approve", Decision::Approve);
                                    if danger {
                                        approve.danger()
                                    } else {
                                        approve.primary()
                                    }
                                    .label("Approve")
                                }),
                        ),
                )
        });
    });
    asked
}
