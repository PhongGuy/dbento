//! Unknown SSH host keys met while connecting: one question per server, however many
//! sessions meet it at once, and the waiting connects go on after the answer.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::ops::Deref;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use tokio::sync::watch;

use crate::confirm::{Ask, Choice};

/// One unknown host key the user is asked about; `key` is what trusting stores.
#[derive(Debug, Clone)]
pub struct HostKeyPrompt<K> {
    /// The connection whose connect met the key.
    pub connection: String,
    /// The SSH server's resolved host name.
    pub host: String,
    /// The SSH server's port.
    pub port: u16,
    /// The key's fingerprint as shown to the user.
    pub fingerprint: String,
    /// The key itself.
    pub key: K,
}

/// What became of a host-key question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKeyAnswer {
    /// The user trusted the key.
    Trusted,
    /// The user declined the key.
    Declined,
    /// Another session met a different key for the same server meanwhile.
    Conflicting,
    /// No prompt handler is registered (or it went away), so nobody was asked.
    Unasked,
}

/// An open question's fingerprint and the channel its waiters hear the answer on.
type Open = (String, watch::Sender<Option<HostKeyAnswer>>);

/// The registered handler's sender and which registration it is, so a replaced handler's
/// late drop cannot unregister its successor.
struct Asks<K> {
    sender: Option<async_channel::Sender<HostKeyPrompt<K>>>,
    generation: u64,
}

#[derive(Default)]
struct State {
    /// Open questions by (host, port), with the fingerprint asked about.
    pending: HashMap<(String, u16), Open>,
    /// (host, port, fingerprint) trusted through this gate, so a session that met the key
    /// before it was stored retries instead of asking again.
    trusted: HashSet<(String, u16, String)>,
}

/// Turns every concurrent unknown-key failure for one SSH server into one question.
pub struct HostKeyGate<K> {
    state: Mutex<State>,
    /// The registered handler; no sender until the shell registers one.
    asks: Mutex<Asks<K>>,
}

/// The registered handler's end: derefs to `async_channel::Receiver<HostKeyPrompt<K>>`
/// (`recv`, `try_recv`). The handler stays registered while this value lives; a clone of the
/// inner receiver does not keep it registered.
pub struct HostKeyPrompts<K> {
    prompts: async_channel::Receiver<HostKeyPrompt<K>>,
    gate: Weak<HostKeyGate<K>>,
    generation: u64,
}

impl<K> Deref for HostKeyPrompts<K> {
    type Target = async_channel::Receiver<HostKeyPrompt<K>>;
    fn deref(&self) -> &Self::Target {
        &self.prompts
    }
}

impl<K> Drop for HostKeyPrompts<K> {
    fn drop(&mut self) {
        if let Some(gate) = self.gate.upgrade() {
            gate.register(None, Some(self.generation));
        }
    }
}

impl<K> HostKeyGate<K> {
    /// A gate with no prompt handler: every wait returns Unasked at once, never blocking.
    pub fn new() -> Arc<Self> {
        let asks = Asks {
            sender: None,
            generation: 0,
        };
        Arc::new(HostKeyGate {
            state: Mutex::new(State::default()),
            asks: Mutex::new(asks),
        })
    }

    /// Registers the prompt handler, replacing any earlier one. Replacing it, or dropping the
    /// returned value, unregisters the old handler and answers every open question Unasked.
    pub fn prompts(self: &Arc<Self>) -> HostKeyPrompts<K> {
        let (asks, prompts) = async_channel::unbounded();
        let generation = self.register(Some(asks), None);
        HostKeyPrompts {
            prompts,
            gate: Arc::downgrade(self),
            generation,
        }
    }

    /// Installs `sender` (None unregisters) unless `only` names a registration that is no
    /// longer current. The outgoing handler can answer nothing more, so every open question
    /// is answered Unasked rather than left to wait forever.
    fn register(
        &self,
        sender: Option<async_channel::Sender<HostKeyPrompt<K>>>,
        only: Option<u64>,
    ) -> u64 {
        let mut state = self.state();
        let mut asks = self.asks.lock().unwrap_or_else(PoisonError::into_inner);
        if only.is_some_and(|g| g != asks.generation) {
            return asks.generation;
        }
        asks.sender = sender;
        asks.generation += 1;
        for (_, (_, open)) in state.pending.drain() {
            let _ = open.send(Some(HostKeyAnswer::Unasked));
        }
        asks.generation
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Asks once per (host, port): the first waiter sends the prompt, later ones with the same
    /// fingerprint share its answer, a later one with another fingerprint gets Conflicting.
    /// An open question whose waiters all went away is asked again by the next wait with the
    /// same fingerprint. A key this gate already trusted gets Trusted at once, unasked.
    pub async fn wait(&self, prompt: HostKeyPrompt<K>) -> HostKeyAnswer {
        let id = (prompt.host.clone(), prompt.port);
        // Checking, sending and registering happen under one lock, so `answer` can never run
        // between them.
        let mut answer = {
            let mut state = self.state();
            let known = (id.0.clone(), id.1, prompt.fingerprint.clone());
            if state.trusted.contains(&known) {
                return HostKeyAnswer::Trusted;
            }
            match state.pending.get(&id) {
                Some((fingerprint, open))
                    if *fingerprint == prompt.fingerprint && open.receiver_count() > 0 =>
                {
                    open.subscribe()
                }
                // Another key's question, stale or not: the handler may still be showing it.
                Some((fingerprint, _)) if *fingerprint != prompt.fingerprint => {
                    return HostKeyAnswer::Conflicting;
                }
                // No question, or this key's question with every waiter gone: ask (again),
                // replacing the stale entry.
                Some(_) | None => {
                    let fingerprint = prompt.fingerprint.clone();
                    // No handler, or one that went away: refuse now rather than wait forever.
                    let asks = self.asks.lock().unwrap_or_else(PoisonError::into_inner);
                    if asks
                        .sender
                        .as_ref()
                        .is_none_or(|a| a.try_send(prompt).is_err())
                    {
                        return HostKeyAnswer::Unasked;
                    }
                    let (open, answer) = watch::channel(None);
                    state.pending.insert(id, (fingerprint, open));
                    answer
                }
            }
        };
        match answer.wait_for(Option::is_some).await {
            Ok(got) => (*got).unwrap_or(HostKeyAnswer::Declined),
            Err(_) => HostKeyAnswer::Declined,
        }
    }

    /// Answers the open question for (host, port) only when it asks about `fingerprint`, and
    /// forgets it; false when no such question is open (stale, replaced or answered).
    pub fn answer(&self, host: &str, port: u16, fingerprint: &str, answer: HostKeyAnswer) -> bool {
        let mut state = self.state();
        let id = (host.to_owned(), port);
        // A dialog answered after its question was replaced must not land on the newer key.
        if state
            .pending
            .get(&id)
            .is_none_or(|(asked, _)| asked != fingerprint)
        {
            return false;
        }
        let Some((asked, open)) = state.pending.remove(&id) else {
            return false;
        };
        if answer == HostKeyAnswer::Trusted {
            state.trusted.insert((host.to_owned(), port, asked));
        }
        let _ = open.send(Some(answer));
        true
    }

    /// Whether a question about `fingerprint` is open for (host, port) with at least one waiter.
    pub fn is_open(&self, host: &str, port: u16, fingerprint: &str) -> bool {
        self.state()
            .pending
            .get(&(host.to_owned(), port))
            .is_some_and(|(asked, open)| asked == fingerprint && open.receiver_count() > 0)
    }

    /// Drops what this gate trusted for (host, port), after a retry still met an unknown key.
    fn untrust(&self, host: &str, port: u16) {
        self.state()
            .trusted
            .retain(|(h, p, _)| !(h == host && *p == port));
    }

    /// How many waits share the open question for (host, port); 0 when none is open.
    pub fn waiting(&self, host: &str, port: u16) -> usize {
        self.state()
            .pending
            .get(&(host.to_owned(), port))
            .map_or(0, |(_, open)| open.receiver_count())
    }
}

/// Why a gated connect did not go through.
#[derive(Debug)]
pub enum Gated<E> {
    /// The connect failed; an unknown key nobody could be asked about lands here too.
    Failed(E),
    /// The user (or a conflicting key) left the server's key untrusted.
    NotTrusted {
        /// The SSH server's resolved host name.
        host: String,
        /// The SSH server's port.
        port: u16,
        /// Declined or Conflicting.
        answer: HostKeyAnswer,
    },
}

/// Runs `attempt`; when `unknown` reads a prompt from its error, waits for the answer and,
/// once trusted, runs `attempt` exactly once more. A retry that still meets an unknown key
/// makes the gate forget that trust, so the next connect asks again. Unasked returns the
/// attempt's own error as `Gated::Failed`, so the unknown-key refusal shows with its hint.
pub async fn with_trust<T, E, K, F, Fut>(
    gate: &HostKeyGate<K>,
    unknown: impl Fn(&E) -> Option<HostKeyPrompt<K>>,
    mut attempt: F,
) -> Result<T, Gated<E>>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    let first = match attempt().await {
        Ok(done) => return Ok(done),
        Err(e) => e,
    };
    let Some(prompt) = unknown(&first) else {
        return Err(Gated::Failed(first));
    };
    let (host, port) = (prompt.host.clone(), prompt.port);
    match gate.wait(prompt).await {
        HostKeyAnswer::Trusted => {}
        HostKeyAnswer::Unasked => return Err(Gated::Failed(first)),
        answer @ (HostKeyAnswer::Declined | HostKeyAnswer::Conflicting) => {
            return Err(Gated::NotTrusted { host, port, answer });
        }
    }
    // One retry only: a trust that did not satisfy the server is forgotten, not looped on.
    attempt().await.map_err(|e| {
        if unknown(&e).is_some() {
            gate.untrust(&host, port);
        }
        Gated::Failed(e)
    })
}

/// The question for an unknown key: Don't trust (0, Enter and Escape) or Trust and connect (1).
pub fn host_key_ask(connection: &str, host: &str, port: u16, fingerprint: &str) -> Ask {
    let choice = |id, label: &'static str| Choice {
        id,
        label: label.into(),
        danger: false,
    };
    Ask {
        title: format!("Trust the SSH host key of {host}:{port}?").into(),
        detail: Some(
            format!(
                "Dbento has not seen this SSH server before ({connection} connects through it). \
                 Check the fingerprint with the server's administrator before you trust it.\n\
                 {fingerprint}"
            )
            .into(),
        ),
        sql: None,
        language: "text",
        choices: vec![
            choice("host-key-reject", "Don't trust"),
            choice("host-key-trust", "Trust and connect"),
        ],
        default: 0,
        cancel: 0,
    }
}

/// Serves `prompts` in order until the channel closes or `decide` returns None. A prompt whose
/// question is no longer open is answered Declined without calling `decide`. `decide`'s answer
/// goes back under the prompt's own fingerprint; None answers Declined and ends the loop,
/// dropping `prompts`, which unregisters the handler.
pub async fn serve_prompts<K>(
    prompts: HostKeyPrompts<K>,
    gate: Arc<HostKeyGate<K>>,
    mut decide: impl AsyncFnMut(&HostKeyPrompt<K>) -> Option<HostKeyAnswer>,
) {
    while let Ok(p) = prompts.recv().await {
        // Every waiter left before the question came up: clear it rather than show it.
        if !gate.is_open(&p.host, p.port, &p.fingerprint) {
            gate.answer(&p.host, p.port, &p.fingerprint, HostKeyAnswer::Declined);
            continue;
        }
        let got = decide(&p).await;
        let answer = got.unwrap_or(HostKeyAnswer::Declined);
        gate.answer(&p.host, p.port, &p.fingerprint, answer);
        if got.is_none() {
            return;
        }
    }
}
