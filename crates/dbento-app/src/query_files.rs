//! The saved-query name dialog.

use std::rc::Rc;

use gpui_kit::{App, SharedString, Window};

use crate::confirm::{TextAsk, ask_text};

/// Why `name` cannot name a saved query, as the dialog says it; None when it can.
pub fn name_problem(name: &str) -> Option<&'static str> {
    let name = name.trim();
    let last = name.rsplit('/').next().unwrap_or(name);
    if name.is_empty() {
        Some("Enter a name.")
    } else if last.eq_ignore_ascii_case(".sql") {
        Some("Enter a name before .sql.")
    } else {
        None
    }
}

/// The file name for `name`: trimmed, with `.sql` appended unless it already ends in any
/// case of `.sql`.
pub fn query_file_name(name: &str) -> String {
    let name = name.trim();
    match name.to_ascii_lowercase().ends_with(".sql") {
        true => name.to_owned(),
        false => format!("{name}.sql"),
    }
}

/// A dialog with an input in `save-name` holding `initial`, focused with its text selected,
/// and `save-cancel` / `save-ok` (Enter). OK on a name with a [`name_problem`] keeps the
/// dialog open and shows it in `save-name-error`; else the receiver yields the trimmed text.
pub fn ask_name(
    initial: &str,
    window: &mut Window,
    cx: &mut App,
) -> async_channel::Receiver<String> {
    let spec = TextAsk {
        title: "Save query".into(),
        hint: "Name, under the connection's folder in queries/".into(),
        ok: "Save".into(),
        initial: initial.to_owned(),
        check: Rc::new(|name| name_problem(name).map(SharedString::from)),
    };
    ask_text(spec, window, cx)
}
