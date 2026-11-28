use std::path::Path;

use dbento_core::{SshEnv, SshHome, StatePaths};
use dbento_pilot::{ssh_env, state_dir};

#[test]
fn the_state_dir_variable_moves_every_file_and_empty_means_unset() {
    let data = Path::new("/d");
    assert_eq!(state_dir(data, None), data);
    assert_eq!(state_dir(data, Some("".into())), data);
    assert_eq!(state_dir(data, Some("/s".into())), Path::new("/s"));
}

#[test]
fn the_test_ssh_home_variable_gives_a_parser_only_home_without_an_agent() {
    let state = StatePaths::in_dir(Path::new("/s"));
    let want = SshEnv::new(SshHome::new("/h", "tester"), state.ssh_known_hosts.clone());
    assert_eq!(
        ssh_env(&state, Some("/h".into())),
        Some(want),
        "the launch env's home replaces the real one, with no ssh program and no agent"
    );
}
