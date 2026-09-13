//! A minimal stand-in for `iced::Task`: the message loop stays an Elm-style
//! `update(Message) -> Task<Message>`, but the effects it hands back are ours.
//!
//! `model` / `message` / `parse` / `tasks` / `update` were the verified half of
//! the old GUI and none of them mention a widget; the only thing that tied them
//! to iced was this type. Keeping its three used constructors (`none`,
//! `perform`, `batch`) spelled the same way is what let the whole message loop
//! move to Slint untouched.
//!
//! A task is a list of futures; each one is spawned on the tokio runtime and its
//! output is posted back to the UI thread as a message (see `super::driver`).

use std::future::Future;
use std::pin::Pin;

/// One effect: a future, already wrapped with the function that turns its
/// output into the message the loop expects.
type Effect<M> = Pin<Box<dyn Future<Output = M> + Send>>;

/// Work for the message loop to run: nothing, one future, or several.
pub struct Task<M> {
    effects: Vec<Effect<M>>,
}

impl<M: 'static> Task<M> {
    /// Nothing to do.
    pub fn none() -> Self {
        Self {
            effects: Vec::new(),
        }
    }

    /// Run `future`, then hand its output to `f` to get the next message.
    pub fn perform<T, F>(future: impl Future<Output = T> + Send + 'static, f: F) -> Self
    where
        T: Send + 'static,
        // `FnOnce` 而不是 `Fn`:`f` 要在 await **之后**才用得上,而一个还活着的 `&F`
        // 会把整个 future 变成非 Send(除非要求 `F: Sync`,那是白白加的限制)。
        F: FnOnce(T) -> M + Send + 'static,
    {
        Self {
            effects: vec![Box::pin(async move {
                let value = future.await;
                f(value)
            })],
        }
    }

    /// Run several tasks at once.
    pub fn batch(tasks: impl IntoIterator<Item = Task<M>>) -> Self {
        Self {
            effects: tasks.into_iter().flat_map(|task| task.effects).collect(),
        }
    }

    /// Take the futures out, leaving the task empty.
    pub fn into_effects(self) -> Vec<Effect<M>> {
        self.effects
    }
}
