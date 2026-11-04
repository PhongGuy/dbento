//! View-free pieces the models share with their views: menu entries and number text.

/// One context menu item: its element id, its label and whether it applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuEntry {
    /// The element id, `grid-menu-*`, `editor-menu-*` or `tree-menu-*`.
    pub id: &'static str,
    /// The text shown.
    pub label: &'static str,
    /// Whether choosing it does something.
    pub enabled: bool,
    /// The effective keys that do the same, shown as a chip; None shows none.
    pub keys: Option<gpui_kit::SharedString>,
}

/// `n` with thousands separators: 1000 reads "1,000".
pub fn group_digits(n: u64) -> String {
    let digits = n.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(c);
    }
    grouped
}
