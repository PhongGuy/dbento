//! The shared session futures stay small whichever engines are built in.
use crate::support;

use std::mem::size_of_val;

use dbento_core::Limits;

// A test or a shell holds these futures on its own stack, so one engine's large arm would make
// every engine pay for it. Budgets sit a quarter above the size without the MongoDB arms.
const EXECUTE_BUDGET: usize = 2_400;
const SCRIPT_BUDGET: usize = 190;

#[tokio::test]
async fn one_engine_does_not_grow_the_futures_every_engine_awaits() {
    let fx = support::sqlite_fixture("CREATE TABLE t (a)", "");
    let mut s = fx.session(Default::default()).await;
    let mut script = s.script("SELECT 1");
    let script_size = size_of_val(&script);
    assert!(
        script_size <= SCRIPT_BUDGET,
        "Script is {script_size} bytes"
    );
    let p = script.next().await.unwrap().unwrap();
    let execute = p.execute(Limits::default());
    let execute_size = size_of_val(&execute);
    assert!(
        execute_size <= EXECUTE_BUDGET,
        "the execute future is {execute_size} bytes"
    );
}
