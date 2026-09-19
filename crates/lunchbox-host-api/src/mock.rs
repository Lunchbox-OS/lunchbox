//! Mock host adapter for testing

use async_trait::async_trait;
use lunchbox_api::{EntryKind, WindowInfo};
use lunchbox_util::SessionId;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

use crate::{
    ExitStatus, HostAdapter, HostCapabilities, HostError, HostEvent, HostHandlePayload, HostResult,
    HostSessionHandle, SpawnOptions, StopMode,
};

/// Mock session state for testing
#[derive(Debug, Clone)]
pub struct MockSession {
    pub session_id: SessionId,
    pub mock_id: u64,
    pub running: bool,
    pub exit_delay: Option<Duration>,
}

/// Mock host adapter for unit/integration testing
pub struct MockHost {
    capabilities: HostCapabilities,
    next_id: AtomicU64,
    sessions: Arc<Mutex<HashMap<u64, MockSession>>>,
    event_tx: mpsc::UnboundedSender<HostEvent>,
    event_rx: Arc<Mutex<Option<mpsc::UnboundedReceiver<HostEvent>>>>,

    /// Whether `set_locked` was last told to cover the screen. The engine
    /// clearing its own flag is not enough — if the host is not also told, the
    /// compositor stays locked while every client reports unlocked.
    pub locked: Arc<Mutex<bool>>,

    /// What `list_windows` reports. Empty by default, which is the "nothing on
    /// screen" case; `set_windows` models a caregiver's work still being up.
    pub windows: Arc<Mutex<Vec<WindowInfo>>>,

    /// Every argv handed to `launch_unsupervised`, in order. Lets a test
    /// assert that administrator mode's gate stops a launch from reaching the
    /// host at all, rather than only that the RPC returned an error.
    pub unsupervised_launches: Arc<Mutex<Vec<Vec<String>>>>,

    /// Configure spawn to fail
    pub fail_spawn: Arc<Mutex<bool>>,

    /// Configure stop to fail
    pub fail_stop: Arc<Mutex<bool>>,

    /// Auto-exit delay (simulates process exiting on its own)
    pub auto_exit_delay: Arc<Mutex<Option<Duration>>>,

    /// How long `stop` blocks before returning, modelling the graceful
    /// SIGTERM-then-SIGKILL wait in the Linux adapter.
    pub stop_blocks_for: Arc<Mutex<Option<Duration>>>,

    /// When set, `stop` leaves the session running and emits no exit event —
    /// the "activity survived the kill" case behind issue #136. A conformant
    /// adapter reports this as [`HostError::StopFailed`] rather than pretending
    /// to have succeeded, so that is what the mock does.
    pub stop_leaves_running: Arc<Mutex<bool>>,

    /// When set, `stop` defers its exit event by this long instead of
    /// emitting it inline. Models the real monitor, whose 100ms poll can
    /// notice the reap only *after* `stop` has already returned.
    pub stop_exit_after: Arc<Mutex<Option<Duration>>>,

    /// Every `set_screen_power` call, in order. The blank is suppressed while
    /// an activity is on screen (issue #144), and "did the compositor get
    /// asked at all" is the only way to tell suppression from a no-op.
    pub screen_power_calls: Arc<Mutex<Vec<bool>>>,
}

impl MockHost {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();

        Self {
            capabilities: HostCapabilities::minimal(),
            next_id: AtomicU64::new(1),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            event_tx: tx,
            event_rx: Arc::new(Mutex::new(Some(rx))),
            locked: Arc::new(Mutex::new(false)),
            windows: Arc::new(Mutex::new(Vec::new())),
            unsupervised_launches: Arc::new(Mutex::new(Vec::new())),
            fail_spawn: Arc::new(Mutex::new(false)),
            fail_stop: Arc::new(Mutex::new(false)),
            auto_exit_delay: Arc::new(Mutex::new(None)),
            stop_blocks_for: Arc::new(Mutex::new(None)),
            stop_leaves_running: Arc::new(Mutex::new(false)),
            stop_exit_after: Arc::new(Mutex::new(None)),
            screen_power_calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Set what the compositor is reported to be showing.
    pub fn set_windows(&self, windows: Vec<WindowInfo>) {
        *self.windows.lock().unwrap() = windows;
    }

    /// Model an activity that ignores every signal: `stop` waits `blocks_for`,
    /// reports success, and the process is still there afterwards.
    pub fn set_unkillable(&self, blocks_for: Duration) {
        *self.stop_blocks_for.lock().unwrap() = Some(blocks_for);
        *self.stop_leaves_running.lock().unwrap() = true;
    }

    /// Model an activity that dies during `stop` but whose exit is only
    /// noticed `after` the call returns, as the real monitor's poll does.
    pub fn set_late_reap(&self, blocks_for: Duration, after: Duration) {
        *self.stop_blocks_for.lock().unwrap() = Some(blocks_for);
        *self.stop_exit_after.lock().unwrap() = Some(after);
    }

    pub fn with_capabilities(mut self, caps: HostCapabilities) -> Self {
        self.capabilities = caps;
        self
    }

    /// Get list of running sessions
    pub fn running_sessions(&self) -> Vec<SessionId> {
        self.sessions
            .lock()
            .unwrap()
            .values()
            .filter(|s| s.running)
            .map(|s| s.session_id.clone())
            .collect()
    }

    /// Simulate process exit
    pub fn simulate_exit(&self, session_id: &SessionId, status: ExitStatus) {
        let sessions = self.sessions.lock().unwrap();
        if let Some(session) = sessions.values().find(|s| &s.session_id == session_id) {
            let handle = HostSessionHandle::new(
                session.session_id.clone(),
                HostHandlePayload::Mock {
                    id: session.mock_id,
                },
            );
            let _ = self.event_tx.send(HostEvent::Exited { handle, status });
        }
    }

    /// Set auto-exit behavior
    pub fn set_auto_exit(&self, delay: Option<Duration>) {
        *self.auto_exit_delay.lock().unwrap() = delay;
    }
}

impl Default for MockHost {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl HostAdapter for MockHost {
    fn capabilities(&self) -> &HostCapabilities {
        &self.capabilities
    }

    async fn set_screen_power(&self, on: bool) -> HostResult<()> {
        self.screen_power_calls.lock().unwrap().push(on);
        Ok(())
    }

    async fn spawn(
        &self,
        session_id: SessionId,
        _entry_kind: &EntryKind,
        _options: SpawnOptions,
    ) -> HostResult<HostSessionHandle> {
        if *self.fail_spawn.lock().unwrap() {
            return Err(HostError::SpawnFailed("Mock spawn failure".into()));
        }

        let mock_id = self.next_id.fetch_add(1, Ordering::SeqCst);

        let session = MockSession {
            session_id: session_id.clone(),
            mock_id,
            running: true,
            exit_delay: *self.auto_exit_delay.lock().unwrap(),
        };

        self.sessions
            .lock()
            .unwrap()
            .insert(mock_id, session.clone());

        let handle =
            HostSessionHandle::new(session_id.clone(), HostHandlePayload::Mock { id: mock_id });

        // If auto-exit is configured, spawn a task to send exit event
        if let Some(delay) = session.exit_delay {
            let tx = self.event_tx.clone();
            let exit_handle = handle.clone();
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                let _ = tx.send(HostEvent::Exited {
                    handle: exit_handle,
                    status: ExitStatus::success(),
                });
            });
        }

        Ok(handle)
    }

    async fn list_windows(&self) -> HostResult<Vec<WindowInfo>> {
        Ok(self.windows.lock().unwrap().clone())
    }

    async fn set_locked(&self, locked: bool) -> HostResult<()> {
        *self.locked.lock().unwrap() = locked;
        Ok(())
    }

    async fn launch_unsupervised(&self, argv: &[String]) -> HostResult<()> {
        self.unsupervised_launches
            .lock()
            .unwrap()
            .push(argv.to_vec());
        Ok(())
    }

    async fn stop(&self, handle: &HostSessionHandle, _mode: StopMode) -> HostResult<()> {
        if *self.fail_stop.lock().unwrap() {
            return Err(HostError::StopFailed("Mock stop failure".into()));
        }

        let mock_id = match handle.payload() {
            HostHandlePayload::Mock { id } => *id,
            _ => return Err(HostError::SessionNotFound),
        };

        if !self.sessions.lock().unwrap().contains_key(&mock_id) {
            return Err(HostError::SessionNotFound);
        }

        // Graceful stops take time in the real adapter; let callers model that
        // so tests can observe what the rest of the system does meanwhile.
        let blocks_for = *self.stop_blocks_for.lock().unwrap();
        if let Some(d) = blocks_for {
            tokio::time::sleep(d).await;
        }

        // The activity shrugged off the kill and is still there.
        if *self.stop_leaves_running.lock().unwrap() {
            return Err(HostError::StopFailed(format!(
                "mock activity {mock_id} survived the stop"
            )));
        }

        self.sessions
            .lock()
            .unwrap()
            .get_mut(&mock_id)
            .expect("checked above")
            .running = false;

        let exited = HostEvent::Exited {
            handle: handle.clone(),
            status: ExitStatus::signaled(15), // SIGTERM
        };

        match *self.stop_exit_after.lock().unwrap() {
            Some(after) => {
                let tx = self.event_tx.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(after).await;
                    let _ = tx.send(exited);
                });
            }
            None => {
                let _ = self.event_tx.send(exited);
            }
        }

        Ok(())
    }

    fn subscribe(&self) -> mpsc::UnboundedReceiver<HostEvent> {
        self.event_rx
            .lock()
            .unwrap()
            .take()
            .expect("subscribe() can only be called once")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[tokio::test]
    async fn mock_spawn_and_stop() {
        let host = MockHost::new();
        let _rx = host.subscribe();

        let session_id = SessionId::new();
        let entry = EntryKind::Process {
            command: "test".into(),
            args: vec![],
            env: HashMap::new(),
            cwd: None,
        };

        let handle = host
            .spawn(session_id.clone(), &entry, SpawnOptions::default())
            .await
            .unwrap();

        assert_eq!(host.running_sessions().len(), 1);

        host.stop(&handle, StopMode::Force).await.unwrap();

        // Session marked as not running
        let sessions = host.sessions.lock().unwrap();
        let session = sessions.values().next().unwrap();
        assert!(!session.running);
    }

    #[tokio::test]
    async fn mock_spawn_failure() {
        let host = MockHost::new();
        let _rx = host.subscribe();
        *host.fail_spawn.lock().unwrap() = true;

        let session_id = SessionId::new();
        let entry = EntryKind::Process {
            command: "test".into(),
            args: vec![],
            env: HashMap::new(),
            cwd: None,
        };

        let result = host
            .spawn(session_id, &entry, SpawnOptions::default())
            .await;

        assert!(result.is_err());
    }
}
