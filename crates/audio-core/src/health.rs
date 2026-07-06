use crate::bus::AudioBus;
use crate::frame::SourceKind;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

/// Health of one source, as observed from the outside (via the bus),
/// rather than self-reported by the source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HealthState {
    /// No frames seen yet since watching started.
    Starting,
    /// A frame of the watched kind arrived within `stale_after`.
    Healthy,
    /// No frame of the watched kind has arrived for longer than `stale_after`.
    Stale,
}

/// Watches the bus for frames of one specific `SourceKind` and reports
/// whether they're still arriving.
///
/// This exists because of a real bug found during Phase 3: the microphone
/// source silently stopped publishing frames after ~35 seconds of
/// concurrent operation with the system-audio helper, even though macOS's
/// own CoreAudio logs confirmed the underlying audio callback kept firing
/// the entire time — the failure was in our own drain loop, invisible to
/// any check that only asked the source "are you running?" (it would have
/// said yes the whole time). Watching frame arrival directly at the bus,
/// instead of trusting each source's internal bookkeeping, catches that
/// whole class of bug regardless of which source or what the root cause
/// is — including bugs neither this project nor its dependencies have hit
/// yet.
///
/// Deliberately *not* implemented here: detecting a source that's still
/// producing frames but with suspiciously all-zero sample content (the
/// Core Audio Process Tap decay failure mode documented in
/// `docs/audio-tap-protocol.md`'s design history). That needs a much
/// longer streak threshold to avoid mistaking real silence for the bug,
/// and inspects sample content rather than message arrival — a
/// deliberately deferred refinement, not an oversight.
pub struct StalenessWatcher {
    kind: SourceKind,
    stale_after: Duration,
}

impl StalenessWatcher {
    pub fn new(kind: SourceKind, stale_after: Duration) -> Self {
        Self { kind, stale_after }
    }

    /// Runs until the bus closes. Calls `on_state_change` on every state
    /// transition (not on every frame), so callers react to edges rather
    /// than polling.
    pub async fn run(&self, bus: AudioBus, mut on_state_change: impl FnMut(HealthState)) {
        let mut rx = bus.subscribe();
        let mut state = HealthState::Starting;
        on_state_change(state);
        let mut last_seen: Option<Instant> = None;

        loop {
            // Recomputed from *our* last-seen frame every iteration, so
            // frames belonging to the other source (which also flow across
            // this same bus) don't reset our staleness clock.
            let wait = match last_seen {
                Some(t) => self.stale_after.saturating_sub(t.elapsed()),
                None => self.stale_after,
            };

            match tokio::time::timeout(wait.max(Duration::from_millis(1)), rx.recv()).await {
                Ok(Ok(frame)) => {
                    if frame.source == self.kind {
                        last_seen = Some(Instant::now());
                        if state != HealthState::Healthy {
                            state = HealthState::Healthy;
                            on_state_change(state);
                        }
                    }
                }
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(broadcast::error::RecvError::Closed)) => break,
                Err(_elapsed) => {
                    if state != HealthState::Stale {
                        state = HealthState::Stale;
                        on_state_change(state);
                    }
                }
            }
        }
    }
}

/// Decides whether an automatic restart should be attempted, based on how
/// many restarts have happened recently — the circuit breaker that keeps a
/// permanently-broken source from crash-looping forever, burning CPU and
/// battery on a long-running background app.
pub struct RestartPolicy {
    max_restarts: u32,
    window: Duration,
    recent_restarts: Vec<Instant>,
}

impl RestartPolicy {
    pub fn new(max_restarts: u32, window: Duration) -> Self {
        Self {
            max_restarts,
            window,
            recent_restarts: Vec::new(),
        }
    }

    /// Call each time a restart is about to be attempted. Returns `true` if
    /// it's within budget (go ahead and restart), `false` if the circuit
    /// breaker has tripped (give up and surface a permanent failure
    /// instead of trying again).
    pub fn record_and_check(&mut self) -> bool {
        let now = Instant::now();
        self.recent_restarts.retain(|t| now.duration_since(*t) < self.window);
        self.recent_restarts.push(now);
        self.recent_restarts.len() <= self.max_restarts as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AudioBus, AudioSource, FakeAudioSource};

    #[tokio::test]
    async fn staleness_watcher_reports_healthy_while_frames_arrive() {
        let bus = AudioBus::new(64);
        let mut source = FakeAudioSource::new(SourceKind::Microphone, vec![0.1; 16], 16_000, 1)
            .with_frame_interval(Duration::from_millis(5));
        source.start(bus.clone()).await.unwrap();

        let watcher = StalenessWatcher::new(SourceKind::Microphone, Duration::from_millis(200));
        let states = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let states_for_watcher = states.clone();

        let watch = tokio::spawn(async move {
            watcher
                .run(bus, |state| states_for_watcher.lock().unwrap().push(state))
                .await;
        });

        tokio::time::sleep(Duration::from_millis(100)).await;
        watch.abort();
        source.stop().await.unwrap();

        let seen = states.lock().unwrap().clone();
        assert!(seen.contains(&HealthState::Starting));
        assert!(seen.contains(&HealthState::Healthy));
        assert!(
            !seen.contains(&HealthState::Stale),
            "should not go stale while frames are actively arriving; saw {seen:?}"
        );
    }

    #[tokio::test]
    async fn staleness_watcher_detects_a_source_that_stops_publishing() {
        let bus = AudioBus::new(64);
        let mut source = FakeAudioSource::new(SourceKind::SystemOutput, vec![0.1; 16], 48_000, 1)
            .with_frame_interval(Duration::from_millis(5));
        source.start(bus.clone()).await.unwrap();

        let watcher = StalenessWatcher::new(SourceKind::SystemOutput, Duration::from_millis(100));
        let states = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let states_for_watcher = states.clone();

        let watch = tokio::spawn(async move {
            watcher
                .run(bus, |state| states_for_watcher.lock().unwrap().push(state))
                .await;
        });

        tokio::time::sleep(Duration::from_millis(50)).await;
        source.stop().await.unwrap(); // frames stop arriving, simulating a stall/crash
        tokio::time::sleep(Duration::from_millis(300)).await;
        watch.abort();

        let seen = states.lock().unwrap().clone();
        assert_eq!(
            seen.last(),
            Some(&HealthState::Stale),
            "expected the watcher to land on Stale after the source stopped; saw {seen:?}"
        );
    }

    #[test]
    fn restart_policy_allows_up_to_the_limit_within_the_window() {
        let mut policy = RestartPolicy::new(3, Duration::from_secs(60));
        assert!(policy.record_and_check()); // 1st
        assert!(policy.record_and_check()); // 2nd
        assert!(policy.record_and_check()); // 3rd
        assert!(!policy.record_and_check()); // 4th — over budget, circuit breaker trips
    }

    #[test]
    fn restart_policy_forgets_restarts_outside_the_window() {
        let mut policy = RestartPolicy::new(1, Duration::from_millis(50));
        assert!(policy.record_and_check());
        std::thread::sleep(Duration::from_millis(60));
        // The first restart has aged out of the window, so this is
        // effectively the "1st" restart again, not the 2nd.
        assert!(policy.record_and_check());
    }
}
