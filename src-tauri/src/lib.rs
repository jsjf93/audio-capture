mod overlay;

use audio_core::{
    AudioBus, AudioSource, HealthState, MicrophoneSource, RestartPolicy, SourceKind,
    StalenessWatcher, SystemOutputSource,
};
use std::time::Duration;
use tauri::{Emitter, Manager};
use tokio::sync::Mutex;

/// Managed Tauri state: the shared bus every source publishes onto, each
/// source itself, and a handle to that source's currently-running
/// supervisor task (if a capture session is active) so it can be aborted
/// cleanly on an intentional stop rather than left to fight the shutdown.
struct AppState {
    bus: AudioBus,
    mic: Mutex<MicrophoneSource>,
    system: Mutex<SystemOutputSource>,
    mic_supervisor: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    system_supervisor: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
}

/// How long a source can go without publishing a frame before it's
/// considered stale. Short on purpose: both sources normally produce
/// frames (or, for system-audio, at minimum a heartbeat) well under a
/// second apart, so a multi-second gap is already a strong, unambiguous
/// signal — see `StalenessWatcher`'s doc comment for why this check exists
/// at all.
const STALE_AFTER: Duration = Duration::from_secs(3);
const MAX_RESTARTS: u32 = 5;
const RESTART_WINDOW: Duration = Duration::from_secs(60);

#[tauri::command]
async fn start_mic_capture(state: tauri::State<'_, AppState>, app: tauri::AppHandle) -> Result<(), String> {
    {
        let mut mic = state.mic.lock().await;
        mic.start(state.bus.clone()).await.map_err(|e| e.to_string())?;
    }
    let handle = tauri::async_runtime::spawn(supervise_mic(app, state.bus.clone()));
    *state.mic_supervisor.lock().await = Some(handle);
    Ok(())
}

#[tauri::command]
async fn stop_mic_capture(state: tauri::State<'_, AppState>) -> Result<(), String> {
    if let Some(handle) = state.mic_supervisor.lock().await.take() {
        handle.abort();
    }
    let mut mic = state.mic.lock().await;
    mic.stop().await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn start_system_capture(state: tauri::State<'_, AppState>, app: tauri::AppHandle) -> Result<(), String> {
    {
        let mut system = state.system.lock().await;
        system.start(state.bus.clone()).await.map_err(|e| e.to_string())?;
    }
    let handle = tauri::async_runtime::spawn(supervise_system(app, state.bus.clone()));
    *state.system_supervisor.lock().await = Some(handle);
    Ok(())
}

#[tauri::command]
async fn stop_system_capture(state: tauri::State<'_, AppState>) -> Result<(), String> {
    if let Some(handle) = state.system_supervisor.lock().await.take() {
        handle.abort();
    }
    let mut system = state.system.lock().await;
    system.stop().await.map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(AppState {
            bus: AudioBus::new(64),
            mic: Mutex::new(MicrophoneSource::new()),
            system: Mutex::new(SystemOutputSource::new()),
            mic_supervisor: Mutex::new(None),
            system_supervisor: Mutex::new(None),
        })
        .manage(overlay::OverlayState::default())
        .setup(|app| {
            // `audio-core` deliberately has no Tauri dependency, so it can't
            // resolve the bundled sidecar path itself — it only knows how
            // to spawn whatever path `AUDIO_TAP_HELPER_PATH` points to (or
            // its own dev-time default if that's unset). Here, in the one
            // place that's allowed to know about Tauri's bundling
            // conventions, point it at the sidecar Tauri copied into
            // Contents/MacOS/ next to the main executable — but only for a
            // release build; a dev build keeps using audio-core's default
            // (the swift-helper package's own debug build), unchanged.
            if !cfg!(debug_assertions) {
                if let Some(sidecar_path) = resolve_bundled_helper_path() {
                    std::env::set_var("AUDIO_TAP_HELPER_PATH", sidecar_path);
                } else {
                    eprintln!(
                        "warning: could not resolve bundled system-audio helper path; \
                         system-audio capture will likely fail to start"
                    );
                }
            }

            // The UI-facing counterpart to Phase 1's console RMS logger:
            // an independent bus subscriber, still completely ignorant of
            // how either source captures its audio, that turns frames into
            // throttled level events for the frontend's two level meters.
            let bus = app.state::<AppState>().bus.clone();
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(level_event_emitter(bus, handle));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            start_mic_capture,
            stop_mic_capture,
            start_system_capture,
            stop_system_capture,
            overlay::open_overlay,
            overlay::close_overlay,
            overlay::set_overlay_interactive_regions,
            overlay::set_overlay_capture_visibility
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[derive(Clone, serde::Serialize)]
struct LevelEvent {
    source: SourceKind,
    rms: f32,
}

/// Emits a `capture://level` event per source, throttled independently per
/// `SourceKind` (mic and system-output frames don't arrive at the same
/// cadence, so a single shared throttle would let one source starve the
/// other's emit slots).
async fn level_event_emitter(bus: AudioBus, app: tauri::AppHandle) {
    let mut rx = bus.subscribe();
    let throttle = Duration::from_millis(100); // ~10Hz per source
    let mut last_mic_emit = std::time::Instant::now() - throttle;
    let mut last_system_emit = std::time::Instant::now() - throttle;

    loop {
        match rx.recv().await {
            Ok(frame) => {
                let last_emit = match frame.source {
                    SourceKind::Microphone => &mut last_mic_emit,
                    SourceKind::SystemOutput => &mut last_system_emit,
                };
                if last_emit.elapsed() >= throttle {
                    let _ = app.emit(
                        "capture://level",
                        LevelEvent {
                            source: frame.source,
                            rms: frame.rms(),
                        },
                    );
                    *last_emit = std::time::Instant::now();
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
}

#[derive(Clone, serde::Serialize)]
struct HealthEvent {
    source: SourceKind,
    state: String,
}

fn emit_health(app: &tauri::AppHandle, source: SourceKind, state: HealthState) {
    let state = match state {
        HealthState::Starting => "starting",
        HealthState::Healthy => "healthy",
        HealthState::Stale => "stale",
    };
    let _ = app.emit(
        "capture://health",
        HealthEvent {
            source,
            state: state.to_string(),
        },
    );
}

fn emit_health_failed(app: &tauri::AppHandle, source: SourceKind) {
    let _ = app.emit(
        "capture://health",
        HealthEvent {
            source,
            state: "failed".to_string(),
        },
    );
}

/// Watches the microphone for staleness and restarts it (up to
/// `MAX_RESTARTS` times per `RESTART_WINDOW`) if it stops publishing
/// frames while a capture session is active. Spawned by
/// `start_mic_capture` and aborted by `stop_mic_capture` — supervision is
/// scoped to an active session so it can never fire before the user has
/// actually started anything.
async fn supervise_mic(app: tauri::AppHandle, bus: AudioBus) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<HealthState>();
    let watcher = StalenessWatcher::new(SourceKind::Microphone, STALE_AFTER);
    let bus_for_watcher = bus.clone();
    let watcher_task =
        tauri::async_runtime::spawn(async move { watcher.run(bus_for_watcher, move |state| { let _ = tx.send(state); }).await });

    let mut policy = RestartPolicy::new(MAX_RESTARTS, RESTART_WINDOW);

    while let Some(state) = rx.recv().await {
        emit_health(&app, SourceKind::Microphone, state);

        if state == HealthState::Stale {
            if !policy.record_and_check() {
                eprintln!("[supervisor:mic] circuit breaker tripped after repeated restarts — giving up");
                emit_health_failed(&app, SourceKind::Microphone);
                break;
            }
            eprintln!("[supervisor:mic] stale — restarting");
            let app_state = app.state::<AppState>();
            let mut mic = app_state.mic.lock().await;
            let _ = mic.stop().await;
            if let Err(e) = mic.start(bus.clone()).await {
                eprintln!("[supervisor:mic] restart failed: {e}");
                emit_health_failed(&app, SourceKind::Microphone);
            } else {
                eprintln!("[supervisor:mic] restarted successfully");
            }
        }
    }

    watcher_task.abort();
}

/// Same as `supervise_mic`, for system-audio. This is also where a
/// `kill -9`'d helper process gets noticed and recovered from: the reader
/// thread inside `SystemOutputSource` sees the pipe close and stops
/// publishing frames, the staleness watcher notices within `STALE_AFTER`,
/// and this restart path spawns a fresh helper — no code here needs to
/// know or care *why* frames stopped arriving.
async fn supervise_system(app: tauri::AppHandle, bus: AudioBus) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<HealthState>();
    let watcher = StalenessWatcher::new(SourceKind::SystemOutput, STALE_AFTER);
    let bus_for_watcher = bus.clone();
    let watcher_task =
        tauri::async_runtime::spawn(async move { watcher.run(bus_for_watcher, move |state| { let _ = tx.send(state); }).await });

    let mut policy = RestartPolicy::new(MAX_RESTARTS, RESTART_WINDOW);

    while let Some(state) = rx.recv().await {
        emit_health(&app, SourceKind::SystemOutput, state);

        if state == HealthState::Stale {
            if !policy.record_and_check() {
                eprintln!("[supervisor:system] circuit breaker tripped after repeated restarts — giving up");
                emit_health_failed(&app, SourceKind::SystemOutput);
                break;
            }
            eprintln!("[supervisor:system] stale — restarting");
            let app_state = app.state::<AppState>();
            let mut system = app_state.system.lock().await;
            let _ = system.stop().await;
            if let Err(e) = system.start(bus.clone()).await {
                eprintln!("[supervisor:system] restart failed: {e}");
                emit_health_failed(&app, SourceKind::SystemOutput);
            } else {
                eprintln!("[supervisor:system] restarted successfully");
            }
        }
    }

    watcher_task.abort();
}

/// In a bundled release build, Tauri copies `externalBin` sidecars into
/// `Contents/MacOS/` right next to the main executable — but *strips* the
/// target-triple suffix on the way in (verified by inspecting an actual
/// build: `src-tauri/binaries/audio-tap-helper-aarch64-apple-darwin` lands
/// in the bundle as plain `audio-tap-helper`). The suffix only matters for
/// how the source file is *named on disk before bundling* (see
/// `scripts/build-helper.sh`), not for where it ends up at runtime.
/// Returns `None` if `current_exe()` fails, which would be unusual enough
/// to warrant surfacing rather than silently falling back to something
/// wrong.
fn resolve_bundled_helper_path() -> Option<std::path::PathBuf> {
    let exe_path = std::env::current_exe().ok()?;
    let dir = exe_path.parent()?;
    Some(dir.join("audio-tap-helper"))
}
