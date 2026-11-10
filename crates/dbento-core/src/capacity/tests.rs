use super::CapacityUnits;

#[test]
fn units_display_as_a_trimmed_decimal() {
    for (units, text) in [
        (0.0, "0"),
        (0.5, "0.5"),
        (1.0, "1"),
        (12.5, "12.5"),
        (0.001, "0.001"),
        (3.25, "3.25"),
        (1234.567, "1234.567"),
    ] {
        assert_eq!(
            CapacityUnits::from_units(units).to_string(),
            text,
            "{units}"
        );
    }
}

#[test]
fn readings_round_to_thousandths_and_never_go_negative() {
    assert_eq!(
        CapacityUnits::from_units(0.0004),
        CapacityUnits::from_units(0.0)
    );
    assert_eq!(CapacityUnits::from_units(0.0005).to_string(), "0.001");
    assert_eq!(CapacityUnits::from_units(-2.0).to_string(), "0");
    assert_eq!(CapacityUnits::from_units(f64::NAN).to_string(), "0");
    assert_eq!(CapacityUnits::from_units(2.5).as_f64(), 2.5);
}

#[test]
fn page_totals_add_without_overflow() {
    let half = CapacityUnits::from_units(0.5);
    assert_eq!(half.saturating_add(half).to_string(), "1");
    let huge = CapacityUnits::from_units(f64::INFINITY);
    assert_eq!(huge.saturating_add(half), huge);
}
