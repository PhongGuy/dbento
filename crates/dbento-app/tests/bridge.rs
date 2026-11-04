use dbento_app::tokio_bridge;
use gpui_kit::TestAppContext;
use std::time::Duration;

#[gpui_kit::test]
async fn spawned_futures_run_on_tokio_and_return_their_value(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(tokio_bridge::init);
    let task = cx.update(|cx| {
        tokio_bridge::spawn(cx, async {
            // A tokio timer panics outside a tokio runtime, so this proves where it ran.
            tokio::time::sleep(Duration::from_millis(5)).await;
            tokio::runtime::Handle::current().runtime_flavor()
        })
    });
    let flavor = task.await.expect("joined");
    assert_eq!(flavor, tokio::runtime::RuntimeFlavor::MultiThread);
}

#[gpui_kit::test]
async fn dropping_the_task_aborts_the_tokio_future(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(tokio_bridge::init);
    let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
    let (held_tx, held_rx) = tokio::sync::oneshot::channel::<()>();
    let task = cx.update(|cx| {
        tokio_bridge::spawn(cx, async move {
            let _held = held_tx;
            let _ = started_tx.send(());
            tokio::time::sleep(Duration::from_secs(60)).await;
        })
    });
    let handle = cx.update(|cx| tokio_bridge::handle(cx));
    handle.block_on(started_rx).expect("started");
    drop(task);
    // A cancelled GPUI task drops its future the next time the scheduler reaches it.
    cx.run_until_parked();
    // An aborted future is dropped, which drops `held_tx` and closes the channel.
    let closed =
        handle.block_on(async { tokio::time::timeout(Duration::from_secs(5), held_rx).await });
    assert!(
        matches!(closed, Ok(Err(_))),
        "the tokio future was not aborted"
    );
}
