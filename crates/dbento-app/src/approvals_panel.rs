//! The approvals panel: MCP requests waiting for an answer, this session's decisions with what
//! became of them, and the command that connects an AI client to the workspace.

use std::path::PathBuf;

use dbento_core::{ApprovalRequest, Decision, GrantState};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    ClipboardItem, Context, ElementId, InteractiveElement, IntoElement, ParentElement, Render,
    SharedString, StatefulInteractiveElement as _, Styled, TestSupportExt, Window, div,
};

use crate::approvals::{CliPath, cli_hint, connect_command};
use crate::status_bar::first_line;
use crate::{theme, ui};

/// What became of a request decided in this session.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The user denied it.
    Denied,
    /// The user approved it; the grant's last reading.
    Granted(GrantState),
}

impl Outcome {
    /// The outcome a fresh `decision` starts with.
    /// The outcome a fresh `decision` starts with; None for a decision this build does not
    /// know, which must not read as either answer.
    pub fn of(decision: Decision) -> Option<Outcome> {
        match decision {
            Decision::Approve => Some(Outcome::Granted(GrantState::Waiting)),
            Decision::Deny => Some(Outcome::Denied),
            _ => None,
        }
    }
}

/// A request this session answered, with its outcome.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decided {
    /// The request as filed.
    pub request: ApprovalRequest,
    /// What became of it.
    pub outcome: Outcome,
}

impl Decided {
    /// `request` with `outcome`.
    pub fn new(request: ApprovalRequest, outcome: Outcome) -> Self {
        Decided { request, outcome }
    }
}

/// The approvals dock: a view of what its owner last set.
pub struct ApprovalsPanel {
    pending: Vec<ApprovalRequest>,
    decided: Vec<Decided>,
    workspace: Option<PathBuf>,
    cli: CliPath,
}

impl ApprovalsPanel {
    /// An empty panel naming `CliPath::current()`.
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self::with_cli(CliPath::current(), cx)
    }

    /// An empty panel naming `cli`.
    pub fn with_cli(cli: CliPath, _cx: &mut Context<Self>) -> Self {
        ApprovalsPanel {
            pending: Vec::new(),
            decided: Vec::new(),
            workspace: None,
            cli,
        }
    }

    /// Shows `pending` oldest first, `decided` newest first, and the connect command for
    /// `workspace` when there is one; the same lists again leave the panel undrawn.
    pub fn set(
        &mut self,
        pending: Vec<ApprovalRequest>,
        decided: Vec<Decided>,
        workspace: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        if (&self.pending, &self.decided, &self.workspace) == (&pending, &decided, &workspace) {
            return;
        }
        (self.pending, self.decided, self.workspace) = (pending, decided, workspace);
        cx.notify();
    }
}

/// The word a decided row ends with.
fn outcome_word(o: Outcome) -> &'static str {
    match o {
        Outcome::Denied => "denied",
        Outcome::Granted(GrantState::Waiting) => "waiting for retry",
        Outcome::Granted(GrantState::Ran) => "ran",
        Outcome::Granted(_) => "expired",
    }
}

// "conn · first line of the SQL", the summary both lists lead with.
fn summary(r: &ApprovalRequest) -> String {
    format!(
        "{} \u{b7} {}",
        r.connection(),
        first_line(r.sql()).unwrap_or_default()
    )
}

impl Render for ApprovalsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (fg, chrome, edge, radius) = (
            theme.sidebar_foreground,
            theme.sidebar,
            theme.border,
            theme.radius,
        );
        let (mono, mono_size) = (theme.mono_font_family.clone(), theme.mono_font_size);
        let dim = theme::dim(cx);
        let row = |id: ElementId, label: String| {
            div()
                .id(id)
                .aria_label(SharedString::from(label.clone()))
                .test_support()
                .px_3()
                .py_1()
                .min_w_0()
                .truncate()
                .text_sm()
                .child(label)
        };
        let empty = |text: &'static str| div().px_3().py_1().text_sm().text_color(dim).child(text);
        let pending: Vec<_> = self
            .pending
            .iter()
            .enumerate()
            .map(|(i, r)| row(("approvals-pending", i).into(), summary(r)).into_any_element())
            .collect();
        let decided: Vec<_> = self
            .decided
            .iter()
            .enumerate()
            .map(|(i, d)| {
                let (text, word) = (summary(&d.request), outcome_word(d.outcome));
                // Only the statement truncates: the outcome word is what the user came to read.
                h_flex()
                    .id(("approvals-decided", i))
                    .aria_label(SharedString::from(format!("{text} \u{b7} {word}")))
                    .test_support()
                    .px_3()
                    .py_1()
                    .gap_1()
                    .text_sm()
                    .child(div().flex_1().min_w_0().truncate().child(text))
                    .child(
                        div()
                            .id(("approvals-decided-word", i))
                            .test_support()
                            .flex_none()
                            .child(format!("\u{b7} {word}")),
                    )
                    .into_any_element()
            })
            .collect();
        let section = |title: &str| ui::panel_header(title, cx);
        // The command an AI client needs, with a button that copies it.
        let connect = self.workspace.as_deref().map(|dir| {
            let command = connect_command(&self.cli, dir);
            // Inside an AppImage the bare command needs a CLI the user has yet to install.
            let hint = cli_hint(&self.cli).map(|hint| {
                div()
                    .id("approvals-connect-hint")
                    .aria_label(hint)
                    .test_support()
                    .text_xs()
                    .text_color(dim)
                    .whitespace_normal()
                    .child(hint)
            });
            v_flex()
                .gap_1()
                .px_3()
                .pb_3()
                .child(
                    div()
                        .id("approvals-connect")
                        .aria_label(SharedString::from(command.clone()))
                        .test_support()
                        .p_2()
                        .rounded(radius)
                        .border_1()
                        .border_color(edge)
                        .bg(theme::editor_fill(cx))
                        .font_family(mono)
                        .text_size(mono_size)
                        .child(command.clone()),
                )
                .children(hint)
                .child(
                    h_flex().child(div().flex_1()).child(
                        Button::new("approvals-copy")
                            .ghost()
                            .xsmall()
                            .label("Copy")
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(command.clone()));
                            }),
                    ),
                )
        });
        let no_pending = pending.is_empty().then(|| empty("No requests waiting."));
        let no_decided = decided.is_empty().then(|| empty("No decisions yet."));
        div()
            .id("approvals-panel")
            .test_support()
            .size_full()
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .rounded(radius)
            .border_1()
            .border_color(edge)
            .bg(chrome)
            .text_color(fg)
            .child(section("Waiting for you"))
            .children(pending)
            .children(no_pending)
            .child(section("This session"))
            .children(decided)
            .children(no_decided)
            .child(section("Connect an AI client"))
            .children(connect)
    }
}
