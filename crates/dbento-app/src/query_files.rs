//! The saved-query name dialog.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use dbento_core::Workspace;
use gpui_kit::{App, SharedString, Window};

use crate::confirm::{TextAsk, TextConfirm, ask_text_confirming};

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

/// Where `name` saves among `conn`'s queries: `queries/<conn>/<file name>`.
pub fn query_rel(conn: &str, name: &str, ext: &str) -> PathBuf {
    Path::new("queries")
        .join(conn)
        .join(query_file_name(name, ext))
}

fn has_extension(name: &str, ext: &str) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(_, e)| e.eq_ignore_ascii_case(ext))
}

/// A dialog with an input in `save-name` holding `initial` for a file ending `.{ext}` in
/// `conn`'s folder of `ws`, focused with its text selected, and `save-cancel` / `save-ok`
/// (Enter). OK on a name with a [`name_problem`] or a path `ws` refuses keeps the dialog open
/// with the reason in `save-name-error`; on an existing file it offers `save-name-confirm`
/// (Overwrite). Else the receiver yields the trimmed text.
pub fn ask_name(
    initial: &str,
    ext: &'static str,
    conn: &str,
    ws: Arc<Workspace>,
    window: &mut Window,
    cx: &mut App,
) -> async_channel::Receiver<String> {
    // Ask about the target itself, not the listing, which skips linked folders.
    let exists = {
        let conn = conn.to_owned();
        move |name: &str| {
            let rel = query_rel(&conn, name, ext);
            (ws.query_exists(&rel), rel)
        }
    };
    let exists = Rc::new(exists);
    let check = {
        let exists = Rc::clone(&exists);
        move |name: &str| match name_problem(name, ext) {
            Some(says) => Some(SharedString::from(says)),
            None => exists(name).0.err().map(|e| e.to_string().into()),
        }
    };
    let confirm = move |name: &str| match exists(name) {
        (Ok(true), rel) => Some(TextConfirm {
            says: format!("{} already exists.", rel.display()).into(),
            accept: "Overwrite".into(),
        }),
        _ => None,
    };
    let spec = TextAsk {
        title: "Save query".into(),
        hint: "Name, under the connection's folder in queries/".into(),
        ok: "Save".into(),
        initial: initial.to_owned(),
        check: Rc::new(check),
    };
    ask_text_confirming(spec, Rc::new(confirm), window, cx)
}
