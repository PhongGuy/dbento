//! Grid navigation: arrows, Home and End over options laid out in rows.
use dbento_app::ui::grid_step;

#[test]
fn left_and_right_step_across_rows_without_wrapping() {
    assert_eq!(grid_step("right", 2, 9, 3), Some(3));
    assert_eq!(grid_step("left", 3, 9, 3), Some(2));
    assert_eq!(grid_step("left", 0, 9, 3), Some(0));
    assert_eq!(grid_step("right", 8, 9, 3), Some(8));
}

#[test]
fn up_and_down_move_a_row_and_stay_put_off_the_grid() {
    assert_eq!(grid_step("down", 1, 9, 3), Some(4));
    assert_eq!(grid_step("up", 4, 9, 3), Some(1));
    assert_eq!(grid_step("up", 2, 9, 3), Some(2));
    assert_eq!(grid_step("down", 7, 9, 3), Some(7));
}

#[test]
fn down_into_a_partial_last_row_only_lands_on_a_card() {
    assert_eq!(grid_step("down", 6, 10, 3), Some(9));
    assert_eq!(grid_step("down", 7, 10, 3), Some(7));
    assert_eq!(grid_step("down", 9, 10, 3), Some(9));
    assert_eq!(grid_step("up", 9, 10, 3), Some(6));
}

#[test]
fn home_and_end_jump_to_the_ends() {
    assert_eq!(grid_step("home", 5, 9, 3), Some(0));
    assert_eq!(grid_step("end", 1, 9, 3), Some(8));
}

#[test]
fn one_column_makes_up_and_down_a_linear_step() {
    assert_eq!(grid_step("down", 1, 3, 1), Some(2));
    assert_eq!(grid_step("up", 1, 3, 1), Some(0));
    assert_eq!(grid_step("up", 0, 3, 1), Some(0));
    assert_eq!(grid_step("down", 2, 3, 1), Some(2));
}

#[test]
fn other_keys_are_not_navigation() {
    assert_eq!(grid_step("tab", 1, 9, 3), None);
    assert_eq!(grid_step("a", 1, 9, 3), None);
}
