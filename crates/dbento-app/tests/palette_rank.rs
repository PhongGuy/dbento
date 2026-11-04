//! The palette's query prefixes and its subsequence ranking.
use dbento_app::palette::{Candidate, Mode, parse_query, rank};

fn c(texts: &[&str]) -> Vec<Candidate> {
    texts
        .iter()
        .map(|t| Candidate {
            text: (*t).to_owned(),
        })
        .collect()
}

#[test]
fn a_prefix_picks_the_mode_and_is_stripped() {
    assert_eq!(parse_query(""), (Mode::All, ""));
    assert_eq!(parse_query("  ord "), (Mode::All, "ord"));
    assert_eq!(parse_query("> reload"), (Mode::Commands, "reload"));
    assert_eq!(parse_query(">"), (Mode::Commands, ""));
    assert_eq!(parse_query("#a"), (Mode::Queries, "a"));
    assert_eq!(parse_query("? cancel"), (Mode::Shortcuts, "cancel"));
    assert_eq!(parse_query("a>b"), (Mode::All, "a>b"));
}

#[test]
fn an_empty_query_keeps_input_order() {
    assert_eq!(rank("", &c(&["b", "a", "c"])), [0, 1, 2]);
}

#[test]
fn an_empty_query_ignores_text_length() {
    assert_eq!(rank("", &c(&["bb", "a"])), [0, 1]);
}

#[test]
fn a_non_subsequence_is_dropped_and_case_is_ignored() {
    assert_eq!(rank("xyz", &c(&["orders"])), Vec::<usize>::new());
    assert_eq!(rank("ORD", &c(&["main.orders", "users"])), [0]);
}

#[test]
fn contiguous_beats_scattered() {
    assert_eq!(rank("ord", &c(&["o_r_d_x", "orders"])), [1, 0]);
}

#[test]
fn a_text_or_word_start_beats_the_middle_of_a_word() {
    assert_eq!(rank("ord", &c(&["border", "orders"])), [1, 0]);
    assert_eq!(rank("ord", &c(&["main.records", "main.orders"])), [1, 0]);
}

#[test]
fn the_best_alignment_counts_not_the_first() {
    // Same length; greedy from the first 'o' scores both as three scattered word starts.
    // Only the best alignment sees the contiguous word-start "ord" at the end of the second.
    assert_eq!(rank("ord", &c(&["abz.o.r.dzz", "z.o.r.d.ord"])), [1, 0]);
}

#[test]
fn ties_go_to_the_shorter_text_then_input_order() {
    assert_eq!(rank("orders", &c(&["orders_archive", "orders"])), [1, 0]);
    assert_eq!(rank("x", &c(&["a.x", "b.x"])), [0, 1]);
}

#[test]
fn a_qualified_query_matches_across_the_dot() {
    assert_eq!(
        rank("pub.ord", &c(&["public.customers", "public.orders"])),
        [1]
    );
}
