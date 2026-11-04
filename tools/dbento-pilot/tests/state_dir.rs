use std::path::Path;

use dbento_pilot::state_dir;

#[test]
fn the_state_dir_variable_moves_every_file_and_empty_means_unset() {
    let data = Path::new("/d");
    assert_eq!(state_dir(data, None), data);
    assert_eq!(state_dir(data, Some("".into())), data);
    assert_eq!(state_dir(data, Some("/s".into())), Path::new("/s"));
}
