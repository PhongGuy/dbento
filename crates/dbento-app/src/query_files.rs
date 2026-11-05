//! The saved-query name dialog.

use std::rc::Rc;

use gpui_kit::{App, SharedString, Window};

use crate::confirm::{TextAsk, ask_text};

/// Why `name` cannot name a saved query with extension `ext`, as the dialog says it; None
/// when it can.
pub fn name_problem(name: &str, ext: &'static str) -> Option<String> {
    let name = name.trim();
    let last = name.rsplit('/').next().unwrap_or(name);
    if name.is_empty() {
        Some("Enter a name.".into())
    } else if last
        .strip_prefix('.')
        .is_some_and(|e| e.eq_ignore_ascii_case(ext))
    {
        Some(format!("Enter a name before .{ext}."))
    } else {
        None
    }
}

/// The file name for `name`: trimmed, with `.{ext}` appended unless it already ends in any
/// case of it.
pub fn query_file_name(name: &str, ext: &str) -> String {
    let name = name.trim();
    match has_extension(name, ext) {
        true => name.to_owned(),
        false => format!("{name}.{ext}"),
    }
}

fn has_extension(name: &str, ext: &str) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(_, e)| e.eq_ignore_ascii_case(ext))
}

/// A dialog with an input in `save-name` holding `initial` for a file ending `.{ext}`, focused
/// with its text selected, and `save-cancel` / `save-ok` (Enter). OK on a name with a
/// [`name_problem`] keeps the dialog open and shows it in `save-name-error`; else the receiver
/// yields the trimmed text.
pub fn ask_name(
    initial: &str,
    ext: &'static str,
    window: &mut Window,
    cx: &mut App,
) -> async_channel::Receiver<String> {
    let spec = TextAsk {
        title: "Save query".into(),
        hint: "Name, under the connection's folder in queries/".into(),
        ok: "Save".into(),
        initial: initial.to_owned(),
        check: Rc::new(move |name| name_problem(name, ext).map(SharedString::from)),
    };
    ask_text(spec, window, cx)
}
