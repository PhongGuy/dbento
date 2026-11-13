//! The Plan result: an explain's nodes as a collapsible tree with a self-time bar per row and
//! the selected node's detail, or the database's raw output in a read-only editor.

use std::collections::HashSet;
use std::ops::Range;
use std::sync::Arc;

use dbento_core::{ExplainMode, ExplainOutput, Plan};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState};
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, ClipboardItem, Context, Entity, FocusHandle, InteractiveElement,
    IntoElement, ParentElement, Render, Role, ScrollStrategy, SharedString,
    StatefulInteractiveElement as _, Styled, TestSupportExt, UniformListScrollHandle, Window, div,
    px, uniform_list,
};

use crate::keymap::{
    TreeCollapse, TreeExpand, TreeSelectFirst, TreeSelectLast, TreeSelectNext, TreeSelectPrev,
};
use crate::settings;
use crate::sidebar::{LEAD, Sidebar};
use crate::tab_model::PlanResult;
use crate::theme;
use crate::ui;

/// A node whose self time is over this share of the execution time is highlighted.
pub const HOT_SHARE: f64 = 0.25;

/// The detail keys shown first, in this order, ahead of buffers and every other key.
const KNOWN: [&str; 8] = [
    "Filter",
    "Index Cond",
    "Hash Cond",
    "Join Filter",
    "Recheck Cond",
    "Sort Key",
    "Group Key",
    "Rows Removed by Filter",
];

/// The self-time bar's full width.
const BAR: f32 = 48.;

/// A tree key: the arrows walk and fold as in the explorer, Toggle folds the selected node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeKey {
    /// The previous visible node.
    Up,
    /// The next visible node.
    Down,
    /// Collapse an open node, else go to its parent.
    Left,
    /// Expand a closed node, else go to its first child.
    Right,
    /// Collapse or expand the selected node.
    Toggle,
    /// The first visible node.
    First,
    /// The last visible node.
    Last,
}

/// The tree's state over one plan: which nodes are collapsed and which is selected.
pub struct PlanTree {
    plan: Arc<Plan>,
    collapsed: HashSet<usize>,
    selected: Option<usize>,
}

impl PlanTree {
    /// Every node expanded and the root selected.
    pub fn new(plan: Arc<Plan>) -> Self {
        let selected = (!plan.nodes.is_empty()).then_some(0);
        PlanTree {
            plan,
            collapsed: HashSet::new(),
            selected,
        }
    }

    /// The plan the tree shows.
    pub fn plan(&self) -> &Plan {
        &self.plan
    }

    /// The node indices shown, in display order: pre-order without collapsed subtrees.
    pub fn visible(&self) -> Vec<usize> {
        // Pre-order puts a subtree right after its root, so skipping runs until depth returns.
        let mut out = Vec::with_capacity(self.plan.nodes.len());
        let mut hidden_below: Option<usize> = None;
        for (ix, node) in self.plan.nodes.iter().enumerate() {
            if hidden_below.is_some_and(|d| node.depth > d) {
                continue;
            }
            hidden_below = self.collapsed.contains(&ix).then_some(node.depth);
            out.push(ix);
        }
        out
    }

    /// The selected node.
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// Selects `ix`, which the caller keeps visible.
    fn select(&mut self, ix: usize) {
        if ix < self.plan.nodes.len() {
            self.selected = Some(ix);
        }
    }

    /// Whether node `ix` has children; in pre-order a first child follows its parent.
    fn parent_node(&self, ix: usize) -> bool {
        self.plan
            .nodes
            .get(ix + 1)
            .is_some_and(|n| n.parent == Some(ix))
    }

    fn collapsed(&self, ix: usize) -> bool {
        self.collapsed.contains(&ix)
    }

    fn toggle(&mut self, ix: usize) {
        if !self.parent_node(ix) {
            return;
        }
        if !self.collapsed.remove(&ix) {
            self.collapsed.insert(ix);
        }
    }

    /// Applies `key` to the selection, the same rules as the explorer's tree keys.
    pub fn key(&mut self, key: TreeKey) {
        let Some(ix) = self.selected else {
            return;
        };
        let visible = self.visible();
        let at = visible.iter().position(|&v| v == ix);
        match key {
            TreeKey::Up => {
                if let Some(&prev) = at
                    .and_then(|p| p.checked_sub(1))
                    .and_then(|p| visible.get(p))
                {
                    self.selected = Some(prev);
                }
            }
            TreeKey::Down => {
                if let Some(&next) = at.and_then(|p| visible.get(p + 1)) {
                    self.selected = Some(next);
                }
            }
            TreeKey::Left if self.parent_node(ix) && !self.collapsed(ix) => self.toggle(ix),
            TreeKey::Left => self.selected = self.plan.nodes[ix].parent.or(Some(ix)),
            TreeKey::Right if self.parent_node(ix) && self.collapsed(ix) => self.toggle(ix),
            TreeKey::Right if self.parent_node(ix) => self.selected = Some(ix + 1),
            TreeKey::Right => {}
            TreeKey::Toggle => self.toggle(ix),
            TreeKey::First => self.selected = visible.first().copied().or(Some(ix)),
            TreeKey::Last => self.selected = visible.last().copied().or(Some(ix)),
        }
    }

    /// Whether node `ix`'s self time is over [`HOT_SHARE`] of the execution time.
    pub fn hot(&self, ix: usize) -> bool {
        self.plan.self_share(ix).is_some_and(|s| s > HOT_SHARE)
    }

    /// The node as Postgres's text format heads it: type, index, relation, alias and join.
    pub fn row_label(&self, ix: usize) -> String {
        let n = &self.plan.nodes[ix];
        let mut s = n.node_type.clone();
        if let Some(index) = &n.index {
            s.push_str(&format!(" using {index}"));
        }
        if let Some(rel) = &n.relation {
            s.push_str(&format!(" on {rel}"));
            if let Some(alias) = n.alias.as_ref().filter(|a| *a != rel) {
                s.push_str(&format!(" {alias}"));
            }
        }
        if let Some(join) = &n.join_type {
            s.push_str(&format!(" ({join})"));
        }
        s
    }

    /// `Key: value` pairs joined by ` · `: the [`KNOWN`] keys, the shared buffers, the rest.
    pub fn detail_line(&self, ix: usize) -> String {
        let n = &self.plan.nodes[ix];
        let pair = |(k, v): &(String, String)| format!("{k}: {v}");
        let known = KNOWN
            .iter()
            .filter_map(|key| n.detail.iter().find(|(k, _)| k == key))
            .map(pair);
        let buffers = [("Shared Hit", n.shared_hit), ("Shared Read", n.shared_read)]
            .into_iter()
            .filter_map(|(k, v)| v.map(|v| format!("{k}: {v}")));
        let rest = (n.detail.iter())
            .filter(|(k, _)| !KNOWN.contains(&k.as_str()))
            .map(pair);
        known
            .chain(buffers)
            .chain(rest)
            .collect::<Vec<_>>()
            .join(" · ")
    }

    /// The row's figures: the estimate `cost a..b · rows r · width w`, then under ANALYZE
    /// `actual t ms · rows r · loops n`; None for a node without costs, as on SQLite.
    pub fn figures(&self, ix: usize) -> Option<String> {
        let n = &self.plan.nodes[ix];
        let total = n.total_cost?;
        let mut s = format!("cost {:.2}..{total:.2}", n.startup_cost.unwrap_or(0.0));
        if let Some(rows) = n.rows {
            s.push_str(&format!(" · rows {}", rows_text(rows)));
        }
        if let Some(width) = n.width {
            s.push_str(&format!(" · width {width}"));
        }
        if let Some(a) = n.actual {
            s.push_str(&format!(
                " · actual {:.3} ms · rows {} · loops {}",
                a.total_ms,
                rows_text(a.rows),
                a.loops
            ));
        }
        Some(s)
    }
}

/// A row count without a fraction when it is whole.
fn rows_text(rows: f64) -> String {
    if rows.fract() == 0.0 {
        format!("{rows:.0}")
    } else {
        format!("{rows:.2}")
    }
}

/// "Planning 1.228 ms · Execution 3.062 ms", either part alone; None with neither.
pub fn header_text(plan: &Plan) -> Option<String> {
    let parts: Vec<String> = [
        ("Planning", plan.planning_ms),
        ("Execution", plan.execution_ms),
    ]
    .into_iter()
    .filter_map(|(name, ms)| ms.map(|ms| format!("{name} {ms:.3} ms")))
    .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// The view's body: the node tree, or the raw output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shown {
    Tree,
    Raw,
}

/// One plan result as the results pane shows it.
pub struct PlanView {
    output: Option<Arc<ExplainOutput>>,
    /// The tree over a plan that parsed; None while pending or when the output did not parse.
    tree: Option<PlanTree>,
    note: Option<&'static str>,
    /// Pending on its ANALYZE confirmation, so nothing is being explained yet.
    waiting: bool,
    shown: Shown,
    raw: Option<Entity<EditorState>>,
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
}

impl PlanView {
    /// A view of `result`: pending without output, Raw when the output did not parse.
    pub fn new(result: &PlanResult, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let output = result.output.clone();
        let tree = output
            .as_ref()
            .and_then(|o| o.plan.as_ref().ok())
            .map(|plan| PlanTree::new(Arc::new(plan.clone())));
        let raw = output.as_ref().map(|o| {
            let text = o.raw.clone();
            cx.new(|cx| {
                let mut state = EditorState::new(window, cx)
                    .line_number(false)
                    .folding(false)
                    .soft_wrap(false)
                    .default_value(text);
                state.set_readonly(true, cx);
                state
            })
        });
        let note = if result.analyze_unavailable {
            Some("SQLite has no ANALYZE timings; showing the query plan")
        } else if result.mode == ExplainMode::Analyze {
            Some("ANALYZE ran in a transaction that was rolled back")
        } else {
            None
        };
        let shown = if tree.is_some() {
            Shown::Tree
        } else {
            Shown::Raw
        };
        PlanView {
            output,
            tree,
            note,
            waiting: false,
            shown,
            raw,
            focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
        }
    }

    /// Marks a pending view as waiting on its ANALYZE confirmation rather than explaining.
    pub fn waiting(mut self, waiting: bool) -> Self {
        self.waiting = waiting;
        self
    }

    /// The tree over the plan; None while pending or when the output did not parse.
    pub fn tree(&self) -> Option<&PlanTree> {
        self.tree.as_ref()
    }

    /// Applies a tree key from the keymap and keeps the selected row in view.
    fn tree_key(&mut self, key: TreeKey, cx: &mut Context<Self>) {
        let Some(tree) = self.tree.as_mut() else {
            return;
        };
        tree.key(key);
        if let Some(pos) =
            (tree.selected()).and_then(|s| tree.visible().iter().position(|&v| v == s))
        {
            self.scroll.scroll_to_item(pos, ScrollStrategy::Nearest);
        }
        cx.notify();
    }

    fn render_rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(tree) = self.tree.as_ref() else {
            return Vec::new();
        };
        let theme = cx.theme();
        let (active, accent, muted, guide, warning, track) = (
            theme.list_active,
            theme.primary,
            theme.muted_foreground,
            theme.border,
            theme.warning,
            theme.secondary,
        );
        let (mono, mono_size) = (theme.mono_font_family.clone(), theme.mono_font_size);
        let dim = theme::dim(cx);
        let height = settings::active_metrics(cx).tree_row;
        let visible = tree.visible();
        let selected = tree.selected();
        visible[range.start.min(visible.len())..range.end.min(visible.len())]
            .iter()
            .map(|&ix| {
                let node = &tree.plan().nodes[ix];
                let label = tree.row_label(ix);
                let on = selected == Some(ix);
                let hot = tree.hot(ix);
                let bg = if on {
                    active
                } else {
                    gpui_kit::transparent_black()
                };
                // Only the chevron folds, so a click on the row itself just selects it.
                let lead = div().w(px(LEAD)).flex_none().flex().justify_center();
                let lead = if tree.parent_node(ix) {
                    lead.id(("plan-node-toggle", ix))
                        .test_support()
                        .cursor_pointer()
                        .child(Sidebar::chevron(!tree.collapsed(ix), muted))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            window.focus(&this.focus, cx);
                            if let Some(t) = this.tree.as_mut() {
                                t.toggle(ix);
                                t.select(ix);
                            }
                            cx.notify();
                        }))
                        .into_any_element()
                } else {
                    lead.into_any_element()
                };
                let bar = tree.plan().self_share(ix).map(|share| {
                    div()
                        .id(("plan-node-bar", ix))
                        .test_support()
                        .flex_none()
                        .w(px(BAR))
                        .h(px(4.))
                        .rounded(px(2.))
                        .bg(track)
                        .child(
                            div()
                                .h_full()
                                .rounded(px(2.))
                                .w(px(BAR * share as f32))
                                .bg(if hot { warning } else { accent }),
                        )
                });
                let marker = hot.then(|| {
                    div()
                        .id(("plan-node-hot", ix))
                        .test_support()
                        .flex_none()
                        .child(ui::icon(ui::Icons::Zap, px(12.), warning))
                });
                Sidebar::row_base(node.depth, guide, height)
                    .id(("plan-node", ix))
                    .role(Role::TreeItem)
                    .aria_label(label.clone())
                    .aria_selected(on)
                    .test_support()
                    .cursor_pointer()
                    .bg(bg)
                    .border_color(if on { accent } else { bg })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        window.focus(&this.focus, cx);
                        if let Some(t) = this.tree.as_mut() {
                            t.select(ix);
                        }
                        cx.notify();
                    }))
                    .child(lead)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .when(hot, |d| d.text_color(warning))
                            .child(label),
                    )
                    .children(marker)
                    .children(tree.figures(ix).map(|f| {
                        div()
                            .flex_none()
                            .font_family(mono.clone())
                            .text_size(mono_size)
                            .text_color(dim)
                            .child(f)
                    }))
                    .children(bar)
                    .into_any_element()
            })
            .collect()
    }

    fn tree_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(tree) = self.tree.as_ref() else {
            return div().into_any_element();
        };
        let border = cx.theme().border;
        let dim = theme::dim(cx);
        let count = tree.visible().len();
        let detail = tree.selected().map(|ix| {
            let line = tree.detail_line(ix);
            (tree.row_label(ix), line)
        });
        let (title, line) = detail.unwrap_or_default();
        let list = uniform_list(
            "plan-tree",
            count,
            cx.processor(|this, range: Range<usize>, _, cx| this.render_rows(range, cx)),
        )
        .track_scroll(&self.scroll)
        .test_support()
        .flex_1()
        .min_h_0();
        // The explorer's tree keys bind in "PlanView", which only the shown tree carries, so
        // Raw's editor keeps its arrows and a hidden tree never moves.
        div()
            .key_context("PlanView")
            .track_focus(&self.focus)
            .on_action(cx.listener(|v, _: &TreeSelectPrev, _, cx| v.tree_key(TreeKey::Up, cx)))
            .on_action(cx.listener(|v, _: &TreeSelectNext, _, cx| v.tree_key(TreeKey::Down, cx)))
            .on_action(cx.listener(|v, _: &TreeSelectFirst, _, cx| v.tree_key(TreeKey::First, cx)))
            .on_action(cx.listener(|v, _: &TreeSelectLast, _, cx| v.tree_key(TreeKey::Last, cx)))
            .on_action(cx.listener(|v, _: &TreeCollapse, _, cx| v.tree_key(TreeKey::Left, cx)))
            .on_action(cx.listener(|v, _: &TreeExpand, _, cx| v.tree_key(TreeKey::Right, cx)))
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(list)
            .child(
                div()
                    .id("plan-detail")
                    .role(Role::Status)
                    .aria_label(format!("{title}\n{line}"))
                    .test_support()
                    .flex_none()
                    .max_h(px(96.))
                    .overflow_y_scroll()
                    .px_3()
                    .py_1()
                    .border_t_1()
                    .border_color(border)
                    .text_xs()
                    .child(
                        div()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .child(div().text_color(dim).child(line)),
            )
            .into_any_element()
    }

    fn raw_body(&self) -> AnyElement {
        let Some(editor) = self.raw.as_ref() else {
            return div().into_any_element();
        };
        div()
            .id("plan-raw")
            .role(Role::Group)
            .test_support()
            .flex_1()
            .min_h_0()
            .child(Editor::new(editor).readonly(true).bordered(false).h_full())
            .into_any_element()
    }
}

impl Render for PlanView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (border, danger, fg) = (theme.border, theme.danger, theme.foreground);
        let dim = theme::dim(cx);
        let root = div()
            .id("plan-view")
            .role(Role::Group)
            .test_support()
            .size_full()
            .flex()
            .flex_col()
            .min_h_0()
            .text_sm();
        let Some(output) = self.output.clone() else {
            let text: SharedString = match self.waiting {
                true => "Waiting for confirmation".into(),
                false => "Explaining…".into(),
            };
            return root.child(
                div()
                    .id("plan-pending")
                    .role(Role::Status)
                    .aria_label(text.clone())
                    .test_support()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(dim)
                    .child(text),
            );
        };
        let header = self.tree.as_ref().and_then(|t| header_text(t.plan()));
        // The toggle only offers Tree when there is a tree to show.
        let toggle = self.tree.is_some().then(|| {
            let weak = cx.entity().downgrade();
            ui::segmented_ids(
                "plan-toggle",
                &[("plan-toggle-tree", "Tree"), ("plan-toggle-raw", "Raw")],
                usize::from(self.shown == Shown::Raw),
                move |ix, _, cx| {
                    let to = [Shown::Tree, Shown::Raw][*ix];
                    let _ = weak.update(cx, |v, cx| {
                        v.shown = to;
                        cx.notify();
                    });
                },
                window,
                cx,
            )
        });
        let raw_text = output.raw.clone();
        let copy = Button::new("plan-copy")
            .ghost()
            .xsmall()
            .icon(ui::Icons::Copy)
            .tooltip("Copy raw output")
            .on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(raw_text.clone()));
            });
        let bar = div()
            .id("plan-header")
            .test_support()
            .flex()
            .flex_none()
            .items_center()
            .gap_3()
            .h(px(32.))
            .px_3()
            .border_b_1()
            .border_color(border)
            // The segmented toggle would shrink and wrap its labels in a narrow 32 px bar.
            .children(toggle.map(|t| div().flex_none().child(t)))
            .children(header.map(|h| {
                div()
                    .flex_none()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_size(cx.theme().mono_font_size)
                    .text_color(fg)
                    .child(h)
            }))
            .children(self.note.map(|note| {
                div()
                    .id("plan-note")
                    .role(Role::Status)
                    .aria_label(note)
                    .test_support()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(dim)
                    .child(note)
            }))
            .child(div().flex_1())
            .child(copy);
        let error = output.plan.as_ref().err().map(|e| {
            let text = format!("The plan could not be read: {e}");
            div()
                .id("plan-parse-error")
                .role(Role::Status)
                .aria_label(text.clone())
                .test_support()
                .flex_none()
                .px_3()
                .py_1()
                .text_xs()
                .text_color(danger)
                .child(text)
        });
        let body = match self.shown {
            Shown::Tree => self.tree_body(cx),
            Shown::Raw => self.raw_body(),
        };
        root.child(bar).children(error).child(body)
    }
}
