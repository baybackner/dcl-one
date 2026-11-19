//! A value read from an outside source and kept: one good read answers for
//! its lifetime; a read that fails can't tell, and that answer is kept
//! [`FAILURE_TTL`] before the next attempt, never an empty value in place of
//! the failure.
//!
//! At most one read runs at a time, in a task of its own: it finishes and
//! keeps its answer (a failure or a panic included) even when every caller
//! that waited on it went away. [`Kept::get`] waits for it; [`Kept::known`]
//! never does.

use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::sync::Notify;
use tokio::time::Instant;

/// How long a failed read answers "can't tell" before the next attempt.
pub const FAILURE_TTL: Duration = Duration::from_secs(30);
/// The whole read, every page, call and retry included.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

struct Cache<T> {
    good: Option<(Instant, T)>,
    failed: Option<Instant>,
    reading: bool,
}

impl<T> Cache<T> {
    fn fresh(&self, ttl: Duration) -> Option<&T> {
        self.good
            .as_ref()
            .filter(|(at, _)| at.elapsed() < ttl)
            .map(|(_, value)| value)
    }

    fn failing(&self) -> bool {
        self.failed.is_some_and(|at| at.elapsed() < FAILURE_TTL)
    }
}

struct Shared<T> {
    cache: Mutex<Cache<T>>,
    settled: Notify,
    ttl: Duration,
}

impl<T> Shared<T> {
    fn cache(&self) -> MutexGuard<'_, Cache<T>> {
        self.cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn settle(&self, read: Option<T>) {
        let mut cache = self.cache();
        match read {
            Some(value) => {
                cache.good = Some((Instant::now(), value));
                cache.failed = None;
            }
            None => {
                cache.failed = Some(Instant::now());
                cache.good.take_if(|(at, _)| at.elapsed() >= self.ttl);
            }
        }
        cache.reading = false;
        drop(cache);
        self.settled.notify_waiters();
    }
}

/// Settles its read as failed if the task ends without settling it.
struct Unsettled<T>(Option<Arc<Shared<T>>>);

impl<T> Drop for Unsettled<T> {
    fn drop(&mut self) {
        if let Some(shared) = self.0.take() {
            shared.settle(None);
        }
    }
}

/// One source's read and its cache; a good read answers for `ttl`.
pub struct Kept<T> {
    shared: Arc<Shared<T>>,
}

impl<T: Clone + Send + 'static> Kept<T> {
    pub fn new(ttl: Duration) -> Self {
        Self {
            shared: Arc::new(Shared {
                cache: Mutex::new(Cache {
                    good: None,
                    failed: None,
                    reading: false,
                }),
                settled: Notify::new(),
                ttl,
            }),
        }
    }

    /// The kept value, waiting for `read` when none is kept; `None` when the
    /// source can't tell.
    pub async fn get<F>(&self, read: impl Fn() -> F) -> Option<T>
    where
        F: Future<Output = Option<T>> + Send + 'static,
    {
        loop {
            let settled = self.shared.settled.notified();
            tokio::pin!(settled);
            settled.as_mut().enable();
            {
                let mut cache = self.shared.cache();
                if let Some(value) = cache.fresh(self.shared.ttl) {
                    return Some(value.clone());
                }
                if cache.failing() {
                    return None;
                }
                if !cache.reading {
                    cache.reading = true;
                    self.spawn(read());
                }
            }
            let _ = tokio::time::timeout(FETCH_TIMEOUT + Duration::from_secs(1), settled).await;
        }
    }

    /// A good read at hand, without waiting: the one kept while it is fresh,
    /// and while the one refresh that follows its expiry runs (a failed
    /// refresh drops an expired read); `None` when no good read is at hand.
    /// Starts the read that is due.
    pub fn known<F>(&self, read: impl FnOnce() -> F) -> Option<T>
    where
        F: Future<Output = Option<T>> + Send + 'static,
    {
        let mut cache = self.shared.cache();
        if cache.fresh(self.shared.ttl).is_none() && !cache.failing() && !cache.reading {
            cache.reading = true;
            self.spawn(read());
        }
        cache
            .good
            .as_ref()
            .filter(|(at, _)| at.elapsed() < self.shared.ttl || cache.reading)
            .map(|(_, value)| value.clone())
    }

    fn spawn(&self, read: impl Future<Output = Option<T>> + Send + 'static) {
        let shared = self.shared.clone();
        tokio::spawn(async move {
            let mut unsettled = Unsettled(Some(shared));
            let read = read.await;
            if let Some(shared) = unsettled.0.take() {
                shared.settle(read);
            }
        });
    }

    #[cfg(test)]
    pub(crate) async fn refresh_with(
        &self,
        read: impl Future<Output = Option<T>> + Send + 'static,
    ) {
        let settled = self.shared.settled.notified();
        tokio::pin!(settled);
        settled.as_mut().enable();
        self.shared.cache().reading = true;
        self.spawn(read);
        settled.await;
    }

    #[cfg(test)]
    pub(crate) fn backdate(&self, by: Duration) {
        let mut cache = self.shared.cache();
        if let Some((at, _)) = cache.good.as_mut() {
            *at = at.checked_sub(by).unwrap();
        }
        if let Some(at) = cache.failed.as_mut() {
            *at = at.checked_sub(by).unwrap();
        }
    }
}
