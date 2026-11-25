//! One question per SSH server however many sessions meet its unknown key, and one retry.
use dbento_app::host_keys::{Gated, HostKeyAnswer, HostKeyGate, HostKeyPrompt, with_trust};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

fn prompt(host: &str, fingerprint: &str) -> HostKeyPrompt<String> {
    HostKeyPrompt {
        connection: "c".into(),
        host: host.into(),
        port: 22,
        fingerprint: fingerprint.into(),
        key: fingerprint.into(),
    }
}

async fn eventually(what: &str, done: impl Fn() -> bool) {
    for _ in 0..1000 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("{what} never happened");
}

fn unknown(e: &&'static str) -> Option<HostKeyPrompt<String>> {
    (*e == "unknown").then(|| prompt("bastion", "SHA256:k"))
}

#[tokio::test]
async fn sessions_meeting_one_unknown_key_share_one_question() {
    let gate = HostKeyGate::new();
    let prompts = gate.prompts();
    let (a, b) = (Arc::clone(&gate), Arc::clone(&gate));
    let first = tokio::spawn(async move { a.wait(prompt("bastion", "SHA256:k")).await });
    let second = tokio::spawn(async move { b.wait(prompt("bastion", "SHA256:k")).await });
    let asked = prompts.recv().await.unwrap();
    assert_eq!(
        (asked.host.as_str(), asked.port, asked.fingerprint.as_str()),
        ("bastion", 22, "SHA256:k"),
        "the prompt"
    );
    eventually("both waiting", || gate.waiting("bastion", 22) == 2).await;
    gate.answer("bastion", 22, "SHA256:k", HostKeyAnswer::Trusted);
    assert_eq!(first.await.unwrap(), HostKeyAnswer::Trusted, "first");
    assert_eq!(second.await.unwrap(), HostKeyAnswer::Trusted, "second");
    assert!(prompts.try_recv().is_err(), "one question only");
    assert_eq!(gate.waiting("bastion", 22), 0, "the question is forgotten");
}

#[tokio::test]
async fn different_servers_are_asked_separately() {
    let gate = HostKeyGate::new();
    let prompts = gate.prompts();
    let (a, b) = (Arc::clone(&gate), Arc::clone(&gate));
    let one = tokio::spawn(async move { a.wait(prompt("one", "SHA256:1")).await });
    let two = tokio::spawn(async move { b.wait(prompt("two", "SHA256:2")).await });
    let mut hosts = vec![
        prompts.recv().await.unwrap().host,
        prompts.recv().await.unwrap().host,
    ];
    hosts.sort();
    assert_eq!(
        hosts,
        vec!["one".to_owned(), "two".to_owned()],
        "two questions"
    );
    gate.answer("one", 22, "SHA256:1", HostKeyAnswer::Trusted);
    gate.answer("two", 22, "SHA256:2", HostKeyAnswer::Declined);
    assert_eq!(one.await.unwrap(), HostKeyAnswer::Trusted, "one");
    assert_eq!(two.await.unwrap(), HostKeyAnswer::Declined, "two");
}

#[tokio::test]
async fn a_different_key_while_asking_conflicts() {
    let gate = HostKeyGate::new();
    let prompts = gate.prompts();
    let a = Arc::clone(&gate);
    let first = tokio::spawn(async move { a.wait(prompt("bastion", "SHA256:k")).await });
    prompts.recv().await.unwrap();
    assert_eq!(
        gate.wait(prompt("bastion", "SHA256:other")).await,
        HostKeyAnswer::Conflicting,
        "never shares the open question"
    );
    assert!(prompts.try_recv().is_err(), "and asks nothing new");
    gate.answer("bastion", 22, "SHA256:k", HostKeyAnswer::Declined);
    assert_eq!(
        first.await.unwrap(),
        HostKeyAnswer::Declined,
        "the first still gets its answer"
    );
}

#[tokio::test]
async fn an_answered_question_is_asked_again_next_time() {
    let gate = HostKeyGate::new();
    let prompts = gate.prompts();
    for _ in 0..2 {
        let a = Arc::clone(&gate);
        let waiter = tokio::spawn(async move { a.wait(prompt("bastion", "SHA256:k")).await });
        prompts.recv().await.unwrap();
        gate.answer("bastion", 22, "SHA256:k", HostKeyAnswer::Declined);
        assert_eq!(waiter.await.unwrap(), HostKeyAnswer::Declined, "declined");
    }
}

#[tokio::test]
async fn with_no_handler_nobody_is_asked_and_nothing_waits() {
    let gate = HostKeyGate::<String>::new();
    assert_eq!(
        gate.wait(prompt("bastion", "SHA256:k")).await,
        HostKeyAnswer::Unasked,
        "none yet"
    );
    drop(gate.prompts());
    assert_eq!(
        gate.wait(prompt("bastion", "SHA256:k")).await,
        HostKeyAnswer::Unasked,
        "gone"
    );
    assert_eq!(gate.waiting("bastion", 22), 0, "nothing left open");
}

#[tokio::test]
async fn with_no_handler_the_unknown_key_refusal_comes_back_at_once() {
    let gate = HostKeyGate::new();
    let tries = AtomicUsize::new(0);
    let got = tokio::time::timeout(
        Duration::from_secs(5),
        with_trust(&gate, unknown, || {
            tries.fetch_add(1, Ordering::SeqCst);
            async { Err::<i32, _>("unknown") }
        }),
    )
    .await
    .expect("never waits");
    assert!(matches!(got, Err(Gated::Failed("unknown"))), "{got:?}");
    assert_eq!(tries.load(Ordering::SeqCst), 1, "no retry");
}

#[tokio::test]
async fn a_trusted_key_retries_the_connect_once() {
    let gate = HostKeyGate::new();
    let prompts = gate.prompts();
    let g = Arc::clone(&gate);
    let answerer = tokio::spawn(async move {
        let p = prompts.recv().await.unwrap();
        g.answer(&p.host, p.port, &p.fingerprint, HostKeyAnswer::Trusted);
    });
    let tries = AtomicUsize::new(0);
    let got = with_trust(&gate, unknown, || {
        let n = tries.fetch_add(1, Ordering::SeqCst);
        async move { if n == 0 { Err("unknown") } else { Ok(7) } }
    })
    .await;
    answerer.await.unwrap();
    assert!(matches!(got, Ok(7)), "{got:?}");
    assert_eq!(tries.load(Ordering::SeqCst), 2, "one retry");
}

#[tokio::test]
async fn a_declined_key_is_not_retried() {
    let gate = HostKeyGate::new();
    let prompts = gate.prompts();
    let g = Arc::clone(&gate);
    let answerer = tokio::spawn(async move {
        let p = prompts.recv().await.unwrap();
        g.answer(&p.host, p.port, &p.fingerprint, HostKeyAnswer::Declined);
    });
    let tries = AtomicUsize::new(0);
    let got = with_trust(&gate, unknown, || {
        tries.fetch_add(1, Ordering::SeqCst);
        async { Err::<i32, _>("unknown") }
    })
    .await;
    answerer.await.unwrap();
    assert!(
        matches!(&got, Err(Gated::NotTrusted { host, port: 22, answer: HostKeyAnswer::Declined }) if host == "bastion"),
        "{got:?}"
    );
    assert_eq!(tries.load(Ordering::SeqCst), 1, "no retry");
}

#[tokio::test]
async fn other_failures_pass_through_without_a_question() {
    let gate = HostKeyGate::new();
    let prompts = gate.prompts();
    let got = with_trust(&gate, unknown, || async { Err::<i32, _>("refused") }).await;
    assert!(matches!(got, Err(Gated::Failed("refused"))), "{got:?}");
    assert!(prompts.try_recv().is_err(), "nothing asked");
}

#[tokio::test]
async fn a_trust_that_did_not_take_retries_once_then_asks_again() {
    let gate = HostKeyGate::new();
    let prompts = gate.prompts();
    let g = Arc::clone(&gate);
    let answerer = tokio::spawn(async move {
        let p = prompts.recv().await.unwrap();
        g.answer(&p.host, p.port, &p.fingerprint, HostKeyAnswer::Trusted);
        prompts
    });
    let tries = AtomicUsize::new(0);
    let got = with_trust(&gate, unknown, || {
        tries.fetch_add(1, Ordering::SeqCst);
        async { Err::<i32, _>("unknown") }
    })
    .await;
    let prompts = answerer.await.unwrap();
    assert!(matches!(got, Err(Gated::Failed("unknown"))), "{got:?}");
    assert_eq!(tries.load(Ordering::SeqCst), 2, "one retry, no loop");
    assert!(prompts.try_recv().is_err(), "one question");
    // The stored key did not satisfy the server, so the next connect asks instead of retrying blind.
    let g = Arc::clone(&gate);
    let next = tokio::spawn(async move { g.wait(prompt("bastion", "SHA256:k")).await });
    prompts.recv().await.unwrap();
    gate.answer("bastion", 22, "SHA256:k", HostKeyAnswer::Declined);
    assert_eq!(next.await.unwrap(), HostKeyAnswer::Declined, "asked again");
}

#[tokio::test]
async fn a_session_late_to_a_trusted_key_retries_without_asking() {
    let gate = HostKeyGate::new();
    let prompts = gate.prompts();
    let a = Arc::clone(&gate);
    let first = tokio::spawn(async move { a.wait(prompt("bastion", "SHA256:k")).await });
    prompts.recv().await.unwrap();
    gate.answer("bastion", 22, "SHA256:k", HostKeyAnswer::Trusted);
    assert_eq!(first.await.unwrap(), HostKeyAnswer::Trusted, "first");
    // This session met the key before it was stored and reports it only after the answer.
    let tries = AtomicUsize::new(0);
    let got = with_trust(&gate, unknown, || {
        let n = tries.fetch_add(1, Ordering::SeqCst);
        async move { if n == 0 { Err("unknown") } else { Ok(7) } }
    })
    .await;
    assert!(matches!(got, Ok(7)), "{got:?}");
    assert_eq!(tries.load(Ordering::SeqCst), 2, "one retry");
    assert!(prompts.try_recv().is_err(), "not asked again");
    let other = Arc::clone(&gate);
    let another = tokio::spawn(async move { other.wait(prompt("bastion", "SHA256:other")).await });
    let asked = prompts.recv().await.unwrap();
    assert_eq!(
        asked.fingerprint, "SHA256:other",
        "a trust covers only the key trusted"
    );
    gate.answer("bastion", 22, "SHA256:other", HostKeyAnswer::Declined);
    assert_eq!(
        another.await.unwrap(),
        HostKeyAnswer::Declined,
        "another key"
    );
}

#[tokio::test]
async fn a_gone_handler_or_a_gone_waiter_never_strands_a_wait() {
    let gate = HostKeyGate::new();
    let prompts = gate.prompts();
    // Every waiter on a question went away (its tab closed): a later wait asks again.
    let a = Arc::clone(&gate);
    let gone = tokio::spawn(async move { a.wait(prompt("bastion", "SHA256:k")).await });
    prompts.recv().await.unwrap();
    gone.abort();
    let _ = gone.await;
    assert_eq!(gate.waiting("bastion", 22), 0, "nobody waits on it");
    let b = Arc::clone(&gate);
    let later = tokio::spawn(async move { b.wait(prompt("bastion", "SHA256:k")).await });
    let asked = prompts.recv().await.unwrap();
    assert_eq!(asked.fingerprint, "SHA256:k", "asked again");
    // A handler that goes away releases what it left open.
    drop(prompts);
    assert_eq!(
        later.await.unwrap(),
        HostKeyAnswer::Unasked,
        "released on drop"
    );
    // So does a handler another registration replaced, and its late drop changes nothing.
    let first = gate.prompts();
    let c = Arc::clone(&gate);
    let pending = tokio::spawn(async move { c.wait(prompt("other", "SHA256:o")).await });
    first.recv().await.unwrap();
    let second = gate.prompts();
    assert_eq!(
        pending.await.unwrap(),
        HostKeyAnswer::Unasked,
        "released on replace"
    );
    assert_eq!(gate.waiting("other", 22), 0, "nothing left open");
    drop(first);
    let d = Arc::clone(&gate);
    let still = tokio::spawn(async move { d.wait(prompt("other", "SHA256:o")).await });
    assert_eq!(
        second.recv().await.unwrap().host,
        "other",
        "the new handler still asks"
    );
    gate.answer("other", 22, "SHA256:o", HostKeyAnswer::Declined);
    assert_eq!(
        still.await.unwrap(),
        HostKeyAnswer::Declined,
        "and its answer lands"
    );
}

#[tokio::test]
async fn a_late_answer_for_another_key_trusts_nothing() {
    let gate = HostKeyGate::new();
    let prompts = gate.prompts();
    let a = Arc::clone(&gate);
    let first = tokio::spawn(async move { a.wait(prompt("bastion", "SHA256:a")).await });
    let old = prompts.recv().await.unwrap();
    assert!(gate.answer("bastion", 22, "SHA256:a", HostKeyAnswer::Declined));
    assert_eq!(
        first.await.unwrap(),
        HostKeyAnswer::Declined,
        "the first question"
    );
    // The server now shows another key; its question is open when the old dialog answers.
    let b = Arc::clone(&gate);
    let second = tokio::spawn(async move { b.wait(prompt("bastion", "SHA256:b")).await });
    prompts.recv().await.unwrap();
    assert!(
        !gate.answer(
            &old.host,
            old.port,
            &old.fingerprint,
            HostKeyAnswer::Trusted
        ),
        "a late answer acts on nothing"
    );
    assert_eq!(
        gate.waiting("bastion", 22),
        1,
        "the newer question is still open"
    );
    assert!(gate.answer("bastion", 22, "SHA256:b", HostKeyAnswer::Declined));
    assert_eq!(
        second.await.unwrap(),
        HostKeyAnswer::Declined,
        "the newer question"
    );
    // Neither key was trusted, so the next wait for either asks again.
    let c = Arc::clone(&gate);
    let again = tokio::spawn(async move { c.wait(prompt("bastion", "SHA256:b")).await });
    assert_eq!(
        prompts.recv().await.unwrap().fingerprint,
        "SHA256:b",
        "asked again"
    );
    assert!(gate.answer("bastion", 22, "SHA256:b", HostKeyAnswer::Declined));
    assert_eq!(again.await.unwrap(), HostKeyAnswer::Declined);
}

#[tokio::test]
async fn a_closed_question_is_not_open() {
    let gate = HostKeyGate::new();
    let prompts = gate.prompts();
    assert!(
        !gate.is_open("bastion", 22, "SHA256:k"),
        "nothing asked yet"
    );
    let a = Arc::clone(&gate);
    let gone = tokio::spawn(async move { a.wait(prompt("bastion", "SHA256:k")).await });
    prompts.recv().await.unwrap();
    eventually("one waiter", || gate.waiting("bastion", 22) == 1).await;
    assert!(
        gate.is_open("bastion", 22, "SHA256:k"),
        "open while someone waits"
    );
    assert!(
        !gate.is_open("bastion", 22, "SHA256:other"),
        "not for another key"
    );
    gone.abort();
    let _ = gone.await;
    assert!(
        !gate.is_open("bastion", 22, "SHA256:k"),
        "every waiter went away"
    );
    let b = Arc::clone(&gate);
    let answered = tokio::spawn(async move { b.wait(prompt("bastion", "SHA256:k")).await });
    prompts.recv().await.unwrap();
    assert!(gate.answer("bastion", 22, "SHA256:k", HostKeyAnswer::Declined));
    assert_eq!(answered.await.unwrap(), HostKeyAnswer::Declined);
    assert!(!gate.is_open("bastion", 22, "SHA256:k"), "answered");
}
