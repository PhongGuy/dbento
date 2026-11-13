//! The window's foot: the open tab's connection, the shown result's row count and whether
//! the row limit cut the result short. Run timing shows in the result footer instead.

use std::time::Duration;

use gpui_kit::base::ObservedElement;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{
    App, Div, Hsla, InteractiveElement, IntoElement, ParentElement, Pixels, Stateful,
    StatefulInteractiveElement, Styled, TestSupportExt, div, px,
};

use crate::link::LinkState;
use crate::query_tab::QueryTab;
use crate::results_pane::{format_rows, shown};
use crate::runner::{StatementEnd, StopReason};
use crate::sidebar::{color_name, tag_color};
use crate::tab_model::{CapView, StatementResult, TabModel};
use crate::{theme, ui};

/// The status bar's height at the window's foot.
pub(crate) const STATUS_HEIGHT: Pixels = px(28.);

/// A duration as people read it: "12 ms" under a second, else "1.25 s".
pub fn format_elapsed(d: Duration) -> String {
    if d < Duration::from_secs(1) {
        format!("{} ms", d.as_millis())
    } else {
        format!("{:.2} s", d.as_secs_f64())
    }
}

/// The first non-blank line of `text`, trimmed; None when every line is blank.
pub fn first_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).find(|l| !l.is_empty())
}

/// The status bar's statement segment: the SQL's first non-blank line, trimmed; for a
/// statement with no SQL that failed (one that failed to prepare), the error's text; else "".
pub fn statement_text(r: &StatementResult) -> String {
    if let Some(line) = first_line(&r.sql) {
        return line.to_owned();
    }
    match r.store.end() {
        Some(StatementEnd::Failed(e)) => e.to_string(),
        _ => String::new(),
    }
}

/// The `status-truncated` text for a result that ended as `end` with cap state `cap`.
/// Parked at the cap it says more rows are available, and while a fetch streams past it
/// there is none; after a park it names how the stream closed; else a truncated result
/// holds its run's cap and a budget stop names the budget.
pub fn truncation_text(end: Option<&StatementEnd>, cap: Option<CapView>) -> Option<String> {
    match (cap, end) {
        (Some(CapView::Fetching), _) => None,
        (Some(CapView::Parked { rows }), _) => Some(format!(
            "row limit reached at {}, more available",
            format_rows(rows)
        )),
        (Some(CapView::Ended { expired: Some(_) }), Some(StatementEnd::Truncated(o))) => Some(
            format!("truncated at {} (stream closed)", format_rows(o.rows)),
        ),
        (
            Some(CapView::Ended { .. }),
            Some(StatementEnd::Stopped {
                rows,
                reason: StopReason::Lost,
            }),
        ) => Some(format!("connection lost at {}", format_rows(*rows))),
        (_, Some(StatementEnd::Truncated(o))) => {
            Some(format!("truncated at {}", format_rows(o.rows)))
        }
        (
            _,
            Some(StatementEnd::Stopped {
                rows,
                reason: StopReason::MemoryBudget,
            }),
        ) => Some(format!("memory budget reached at {}", format_rows(*rows))),
        _ => None,
    }
}

/// The statement the status chip describes: the run's first failed statement, else the
/// skipped one its run stopped at, else the shown one, else the last; None for an empty run.
pub fn status_subject(results: &[StatementResult], shown: Option<usize>) -> Option<usize> {
    let failed = |r: &StatementResult| matches!(r.store.end(), Some(StatementEnd::Failed(_)));
    let skipped = |r: &StatementResult| matches!(r.store.end(), Some(StatementEnd::Skipped));
    (results.iter().position(failed))
        .or_else(|| results.iter().position(skipped))
        .or(shown)
        .or(results.len().checked_sub(1))
}

/// The status chip's icon for its statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusMark {
    /// The statement failed.
    Failed,
    /// The run is still going.
    Running,
    /// The statement was skipped or stopped by the user: neither a failure nor a success.
    Neutral,
    /// It finished.
    Done,
}

/// The mark for a statement that ended as `end`, while the run is `running` or not.
pub fn status_mark(end: Option<&StatementEnd>, running: bool) -> StatusMark {
    match end {
        Some(StatementEnd::Failed(_)) => StatusMark::Failed,
        _ if running => StatusMark::Running,
        Some(
            StatementEnd::Skipped
            | StatementEnd::Stopped {
                reason: StopReason::User,
                ..
            },
        ) => StatusMark::Neutral,
        _ => StatusMark::Done,
    }
}

/// The run's time: the sum of its ended statements' times; None until one has ended.
pub fn run_elapsed(results: &[StatementResult]) -> Option<Duration> {
    let mut times = results.iter().filter_map(|r| r.elapsed).peekable();
    times.peek().is_some().then(|| times.sum())
}

/// The bar for the open tab and its model, as chips on the frame: the connection and the
/// last statement on the left, the session note and the production flag on the right.
/// Without a tab it reads `idle`, the explorer's selected connection, as "name · state" (else
/// "No connection") and "Ready". The tab's display config gives the colour and its session
/// config the name and target. `closing` says the window is waiting for the sessions to close.
pub fn status_bar(
    tab: Option<(&QueryTab, &TabModel)>,
    idle: Option<(&str, &LinkState)>,
    closing: bool,
    cx: &App,
) -> impl IntoElement + use<> {
    let dim = theme::dim(cx);
    let theme = cx.theme();
    let (danger, warning, success, frame) =
        (theme.danger, theme.warning, theme.success, theme.background);
    let (mono, mono_size) = (theme.mono_font_family.clone(), theme.mono_font_size);
    let figure = |id: &'static str, text: String| {
        div()
            .id(id)
            .aria_label(text.clone())
            .test_support()
            .flex_none()
            .whitespace_nowrap()
            .font_family(mono.clone())
            .text_size(mono_size)
            .child(text)
    };
    // The left side shrinks, statement first and connection only once that is gone;
    // the figures and the right side never do.
    let mut left = div().flex().items_center().gap_1p5().flex_1().min_w_0();
    let mut right = div().flex().flex_none().items_center().gap_1p5();
    let Some((tab, model)) = tab else {
        let text = idle.map_or("No connection".to_owned(), |(name, link)| {
            format!("{name} \u{B7} {}", link.word())
        });
        let none = ui::chip("status-connection", text, cx).text_color(dim);
        return bar(frame)
            .child(left.child(none))
            .child(right.child(ui::chip("status-session", "Ready", cx)));
    };
    let (conn, session) = (tab.conn(), tab.session_conn());
    let label = format!("{} \u{B7} {}", session.name(), session.display_target());
    let mut connection = ui::chip_base(cx)
        .id("status-connection")
        .aria_label(label.clone())
        .test_support()
        .min_w_0();
    connection.style().flex_shrink = Some(0.01);
    if let Some(color) = conn.effective_color() {
        connection = connection.child(
            div()
                .id("status-color")
                .aria_label(color_name(color))
                .test_support()
                .size(px(8.))
                .flex_none()
                .rounded_full()
                .bg(tag_color(color)),
        );
    }
    left = left.child(connection.child(div().min_w_0().truncate().child(label)));
    // Where a USE took the session, beside the configured connection it no longer matches.
    if let Some(text) = model.current_database() {
        left = left.child(
            ui::chip("status-database", text.to_owned(), cx)
                .flex_none()
                .border_color(warning)
                .text_color(warning),
        );
    }
    let results = model.results();
    let shown_ix = shown(results);
    let shown = shown_ix.map(|ix| &results[ix]);
    // A failure names its own statement; else the grid's result, else what ran last.
    if let Some(ix) = status_subject(results, shown_ix) {
        let r = &results[ix];
        let mark = match status_mark(r.store.end(), model.running()) {
            StatusMark::Failed => ui::icon(ui::Icons::CircleX, px(12.), danger),
            StatusMark::Running => ui::icon(ui::Icons::Clock, px(12.), dim),
            StatusMark::Neutral => ui::icon(ui::Icons::CircleStop, px(12.), dim),
            StatusMark::Done => ui::icon(ui::Icons::CircleCheck, px(12.), success),
        };
        let mut last = ui::chip_base(cx)
            .gap_2()
            .flex_shrink(1.)
            .min_w_0()
            .child(mark)
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_family(mono.clone())
                    .text_size(mono_size)
                    .text_color(dim)
                    .child(statement_text(r)),
            );
        // Before its first rows a running statement has nothing to count yet.
        if shown_ix == Some(ix) {
            let rows = match model.awaiting_rows(r.stmt) {
                Some(_) => "Running\u{2026}".to_owned(),
                None => format_rows(r.store.rows()),
            };
            last = last.child(figure("status-rows", rows));
        }
        left = left.child(last);
    }
    if let Some(text) = tab.results_pane().read(cx).was_text(cx) {
        left = left.child(
            div()
                .id("status-was")
                .aria_label(text.clone())
                .test_support()
                .min_w_0()
                .truncate()
                .text_color(dim)
                .child(text),
        );
    }
    // A browse that can continue lost nothing, so it reads as paused, like its footer.
    let paused = shown
        .filter(|r| r.stmt == 0 && model.continuation().is_some())
        .map(|r| format!("paused at {}", format_rows(r.store.rows())));
    let cut = truncation_text(
        shown.and_then(|r| r.store.end()),
        shown.and_then(|r| model.cap_view(r.stmt)),
    );
    if let Some(text) = paused {
        left = left.child(ui::chip("status-paused", text, cx).text_color(dim));
    } else if let Some(text) = cut {
        left = left.child(
            ui::chip("status-truncated", text, cx)
                .border_color(warning)
                .text_color(warning),
        );
    }
    // A run's closing or draining outranks its last notice.
    let note = if closing {
        Some("closing sessions")
    } else if model.settling() {
        Some("discarding remaining rows")
    } else {
        model.notice()
    };
    if let Some(text) = note {
        right = right.child(ui::chip("status-session", text.to_owned(), cx));
    }
    if conn.is_production() {
        right = right.child(ui::production_chip(cx));
    }
    bar(frame).child(left).child(right)
}

// The 28 px strip on the frame that holds the chips.
fn bar(frame: Hsla) -> ObservedElement<Stateful<Div>> {
    div()
        .id("status-bar")
        .test_support()
        .flex()
        .flex_none()
        .items_center()
        .justify_between()
        .gap_1p5()
        .h(STATUS_HEIGHT)
        .px(px(6.))
        .bg(frame)
        .text_xs()
}
