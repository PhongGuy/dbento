//! The result footer's capacity figure.
use dbento_app::results_pane::capacity_text;
use dbento_app::runner::StatementEnd;
use dbento_core::{CapacityUnits, Outcome};

fn with(units: Option<u64>) -> Outcome {
    let mut o = Outcome::complete(3);
    o.consumed_capacity = units.map(CapacityUnits::from_thousandths);
    o
}

#[test]
fn the_footer_names_the_units_only_when_reported() {
    assert_eq!(
        capacity_text(Some(&StatementEnd::Complete(with(Some(1_000))))).as_deref(),
        Some("1 capacity unit")
    );
    assert_eq!(
        capacity_text(Some(&StatementEnd::Truncated(with(Some(12_500))))).as_deref(),
        Some("12.5 capacity units")
    );
    assert_eq!(
        capacity_text(Some(&StatementEnd::Complete(with(None)))),
        None
    );
    assert_eq!(capacity_text(Some(&StatementEnd::Skipped)), None);
    assert_eq!(capacity_text(None), None);
}
