use super::*;

fn row(fields: &[(&str, Wire)]) -> Wire {
    Wire::Object(
        fields
            .iter()
            .map(|(name, value)| ((*name).to_owned(), value.clone()))
            .collect(),
    )
}

fn names(columns: &[ColumnInfo]) -> Vec<&str> {
    columns.iter().map(ColumnInfo::name).collect()
}

fn types(columns: &[ColumnInfo]) -> Vec<&str> {
    columns.iter().map(ColumnInfo::type_name).collect()
}

#[test]
fn no_rows_means_no_columns() {
    let (shape, columns) = Shape::of(&[], false, true);
    assert!(columns.is_empty());
    assert_eq!(shape.width(), 0);
}

#[test]
fn id_comes_first_then_the_sorted_union() {
    let rows = [
        row(&[("b", Wire::Int(1)), ("id", Wire::Int(1))]),
        row(&[("a", Wire::Int(1)), ("id", Wire::Int(2))]),
    ];
    let (_, columns) = Shape::of(&rows, false, true);
    assert_eq!(names(&columns), ["id", "a", "b"]);
}

#[test]
fn a_type_is_the_common_one_ignoring_nulls_or_mixed() {
    let rows = [
        row(&[("a", Wire::Int(1)), ("b", Wire::Int(1)), ("c", Wire::None)]),
        row(&[
            ("a", Wire::None),
            ("b", Wire::Text("x".to_owned())),
            ("c", Wire::Null),
        ]),
    ];
    let (_, columns) = Shape::of(&rows, false, true);
    assert_eq!(types(&columns), ["int", "mixed", "any"]);
}

#[test]
fn an_unfinished_stream_keeps_a_trailing_column_for_later_fields() {
    let rows = [row(&[("a", Wire::Int(1))])];
    let (shape, columns) = Shape::of(&rows, false, false);
    assert_eq!(names(&columns), ["a", MORE_FIELDS]);
    assert_eq!(shape.width(), 2);
    let plain = shape.cells(&rows[0]);
    assert_eq!(plain[1], Cell::Null);
    let later = row(&[("a", Wire::Int(2)), ("z", Wire::Int(3))]);
    let cells = shape.cells(&later);
    assert_eq!(cells[1], Cell::Text(Cow::Borrowed("{z: 3}")));
    let scalar = Wire::Int(7);
    let cells = shape.cells(&scalar);
    assert_eq!(cells[0], Cell::Null, "a later row that is not an object");
    assert_eq!(cells[1], Cell::Text(Cow::Borrowed("7")));
}

#[test]
fn a_row_that_is_not_an_object_or_a_single_statement_is_one_value_column() {
    let rows = [row(&[("a", Wire::Int(1))]), Wire::Int(2)];
    let (shape, columns) = Shape::of(&rows, false, false);
    assert_eq!(names(&columns), [VALUE]);
    assert_eq!(types(&columns), ["mixed"]);
    assert_eq!(shape.width(), 1);
    let (_, columns) = Shape::of(&rows[..1], true, true);
    assert_eq!(names(&columns), [VALUE]);
    assert_eq!(types(&columns), ["object"]);
}

#[test]
fn a_missing_field_is_null_and_a_present_one_keeps_its_number_bit() {
    let rows = [
        row(&[("a", Wire::Int(1))]),
        row(&[("b", Wire::Text("x".to_owned()))]),
    ];
    let (shape, _) = Shape::of(&rows, false, true);
    let cells = shape.cells(&rows[0]);
    assert!(cells[0].is_number());
    assert_eq!(cells[1], Cell::Null);
}
