//! Dbento desktop app: a GPUI shell over `dbento-core`.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod app;
pub mod approvals;
pub mod approvals_panel;
#[cfg(feature = "bench")]
pub mod bench;
pub mod bench_driver;
pub mod browse;
pub mod catalog;
pub mod completion;
pub mod completion_popup;
pub mod confirm;
pub mod connection_dialog;
pub mod copy;
pub mod definition_panel;
pub mod drafts;
pub mod editor_completion;
pub mod explorer;
pub mod export;
pub mod format;
pub mod grid;
pub mod grid_menu;
pub mod history;
pub mod history_panel;
pub mod identity;
pub mod inspector;
pub mod keymap;
pub mod labels;
pub mod link;
pub mod nav;
pub mod palette;
mod parks;
pub mod persist;
pub mod plan_view;
pub mod query_files;
pub mod query_tab;
pub mod recent_tabs;
pub mod record;
pub mod record_form;
pub mod restore;
pub mod result_body;
pub mod results;
pub mod results_pane;
pub mod run_target;
#[allow(missing_docs)]
pub mod runner;
pub mod schema_tree;
pub mod settings;
pub mod settings_dialog;
pub mod settings_store;
pub mod shell;
pub mod sidebar;
pub mod smooth_scroll;
pub mod staged;
pub mod statements;
pub mod status_bar;
pub mod tab_model;
pub mod theme;
pub mod tokio_bridge;
pub mod ui;
pub mod user_keymap;
