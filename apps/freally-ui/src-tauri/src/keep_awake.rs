//! FFM-M05 — automatic sleep-inhibit while jobs run.
//!
//! A reference-counted wrapper over `freally_platform::acquire_keep_awake`
//! so many concurrently-running jobs share one OS wake-lock: the first
//! `arm()` acquires it, later `arm()`s just bump a counter, and the lock
//! is released only when the count returns to zero (the last active job
//! finished). The runner arms once at job start and disarms once at job
//! end, each guarded by the `keep_awake_during_jobs` setting.
//!
//! Separate from `AppState::wake_lock` (the mobile "Keep desktop awake"
//! toggle) so the two hold independent locks and neither releases the
//! other's.

use std::sync::{Arc, Mutex};

use freally_platform::WakeLock;

/// RAII guard: [`arm`](KeepAwake::arm)s on construction and
/// [`disarm`](KeepAwake::disarm)s on drop, so a running job's
/// keep-awake is released on every exit path (return, error, unwind).
pub struct KeepAwakeGuard(Arc<KeepAwake>);

impl KeepAwakeGuard {
    pub fn arm(ka: Arc<KeepAwake>) -> Self {
        ka.arm();
        Self(ka)
    }
}

impl Drop for KeepAwakeGuard {
    fn drop(&mut self) {
        self.0.disarm();
    }
}

/// Reference-counted keep-awake coordinator. Cheap to clone the `Arc`
/// that holds it; all state lives behind one mutex.
#[derive(Default)]
pub struct KeepAwake {
    inner: Mutex<State>,
}

#[derive(Default)]
struct State {
    count: u32,
    lock: Option<WakeLock>,
}

impl KeepAwake {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one active job. Acquires the OS wake-lock on the
    /// 0 → 1 transition; a platform failure is swallowed (best-effort,
    /// like the mobile toggle) but the count still rises so a matching
    /// `disarm` stays balanced.
    pub fn arm(&self) {
        let mut st = self.inner.lock().expect("keep-awake poisoned");
        st.count += 1;
        if st.count == 1 && st.lock.is_none() {
            match freally_platform::acquire_keep_awake() {
                Ok(lock) => st.lock = Some(lock),
                Err(e) => eprintln!("[keep-awake] could not inhibit sleep: {e}"),
            }
        }
    }

    /// Deregister one active job. Releases the wake-lock on the
    /// 1 → 0 transition. Balanced against `arm`; an unmatched `disarm`
    /// (count already 0) is a no-op.
    pub fn disarm(&self) {
        let mut st = self.inner.lock().expect("keep-awake poisoned");
        if st.count == 0 {
            return;
        }
        st.count -= 1;
        if st.count == 0 {
            // Drop releases the OS assertion.
            st.lock.take();
        }
    }

    /// Whether the wake-lock is currently held (for tests / status).
    #[cfg(test)]
    pub fn is_active(&self) -> bool {
        self.inner
            .lock()
            .expect("keep-awake poisoned")
            .lock
            .is_some()
    }

    #[cfg(test)]
    pub fn count(&self) -> u32 {
        self.inner.lock().expect("keep-awake poisoned").count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refcount_holds_until_last_disarm() {
        let ka = KeepAwake::new();
        assert_eq!(ka.count(), 0);
        ka.arm();
        ka.arm();
        assert_eq!(ka.count(), 2);
        // Whether the OS wake-lock actually acquired is best-effort and
        // environment-dependent (a headless CI host may refuse it), so
        // we only assert the deterministic refcount here...
        ka.disarm();
        assert_eq!(ka.count(), 1);
        ka.disarm();
        assert_eq!(ka.count(), 0);
        // ...and that returning to zero always releases whatever lock
        // was held (never leaks a `Some`).
        assert!(!ka.is_active());
    }

    #[test]
    fn unbalanced_disarm_is_a_noop() {
        let ka = KeepAwake::new();
        ka.disarm();
        assert_eq!(ka.count(), 0);
    }
}
