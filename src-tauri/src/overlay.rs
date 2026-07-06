//! The suggestion overlay window: transparent, always-on-top (including
//! over fullscreen call apps in their own Spaces), hidden from screen
//! capture by default, and click-through everywhere except its own cards.
//!
//! The native techniques here were proven by a dedicated spike (July 2026)
//! before this module existed; the three raw NSWindow properties and the
//! cursor-polling model are documented at their call sites below.
//!
//! Until the Tier-1 cue agent exists, a mock generator publishes canned
//! suggestions on the same `overlay://suggestion` event the real agent
//! will use — the frontend can't tell the difference, which is the point:
//! swapping mock for real later means deleting `spawn_mock_suggestions`
//! and nothing else.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{Emitter, Manager};

pub const OVERLAY_LABEL: &str = "overlay";

/// Window-relative, logical-pixel rectangles the frontend reports as
/// interactive (cards, the handle pill). The cursor being inside any of
/// them makes the window accept mouse events; everywhere else is
/// click-through.
#[derive(Clone, Copy, serde::Deserialize)]
pub struct Region {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Default)]
pub struct OverlayState {
    regions: Mutex<Vec<Region>>,
    /// Handle to the current overlay session's liveness flag. Re-opening
    /// the overlay flips the old flag off so the previous session's
    /// background threads (cursor poller, mock generator) can't outlive it
    /// and double up against the new window.
    session: Mutex<Option<Arc<AtomicBool>>>,
}

impl OverlayState {
    fn begin_session(&self) -> Arc<AtomicBool> {
        let flag = Arc::new(AtomicBool::new(true));
        let mut session = self.session.lock().unwrap();
        if let Some(old) = session.replace(flag.clone()) {
            old.store(false, Ordering::SeqCst);
        }
        flag
    }

    fn end_session(&self) {
        if let Some(old) = self.session.lock().unwrap().take() {
            old.store(false, Ordering::SeqCst);
        }
    }
}

#[tauri::command]
pub fn set_overlay_interactive_regions(state: tauri::State<'_, OverlayState>, regions: Vec<Region>) {
    *state.regions.lock().unwrap() = regions;
}

#[tauri::command]
pub fn close_overlay(app: tauri::AppHandle, state: tauri::State<'_, OverlayState>) {
    state.end_session();
    if let Some(window) = app.get_webview_window(OVERLAY_LABEL) {
        let _ = window.close();
    }
}

/// Demo mode: flips the window between invisible-to-capture (the default;
/// `NSWindowSharingNone` = 0) and capturable (`NSWindowSharingReadOnly`
/// = 1). Runtime-mutable, so it's a user toggle, not a build decision.
#[tauri::command]
pub fn set_overlay_capture_visibility(app: tauri::AppHandle, visible: bool) {
    #[cfg(target_os = "macos")]
    {
        if let Some(window) = app.get_webview_window(OVERLAY_LABEL) {
            let w = window.clone();
            let _ = window.run_on_main_thread(move || {
                use objc2::msg_send;
                use objc2::runtime::AnyObject;

                let Ok(raw) = w.ns_window() else { return };
                let ns_window = raw as *mut AnyObject;
                let sharing: usize = if visible { 1 } else { 0 };
                unsafe {
                    let _: () = msg_send![ns_window, setSharingType: sharing];
                }
            });
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, visible);
    }
}

/// Deliberately a sync command: sync commands run on the main thread, and
/// AppKit requires windows to be created there.
#[tauri::command]
pub fn open_overlay(app: tauri::AppHandle, state: tauri::State<'_, OverlayState>) -> Result<(), String> {
    if let Some(existing) = app.get_webview_window(OVERLAY_LABEL) {
        let _ = existing.set_focus();
        return Ok(());
    }

    let window = tauri::WebviewWindowBuilder::new(
        &app,
        OVERLAY_LABEL,
        // Same index.html as the main window; the frontend branches on the
        // window label (see src/main.tsx).
        tauri::WebviewUrl::App("index.html".into()),
    )
    .title("Overlay")
    .inner_size(380.0, 640.0)
    .position(80.0, 80.0)
    .transparent(true) // pairs with `macOSPrivateApi: true` in tauri.conf.json
    .decorations(false)
    .always_on_top(true)
    .visible_on_all_workspaces(true)
    .focused(false) // an overlay must never yank focus away from the call
    .build()
    .map_err(|e| e.to_string())?;

    apply_native_overlay_behavior(&window);

    let session = state.begin_session();
    spawn_cursor_poller(app.clone(), session.clone());
    spawn_mock_suggestions(app, session);
    Ok(())
}

/// The three NSWindow properties Tauri's builder has no knobs for, applied
/// via raw Objective-C messages. Must run on the main thread (AppKit rule).
///
/// Constants inlined from the AppKit headers:
///   canJoinAllSpaces    = 1 << 0  (NSWindowCollectionBehavior)
///   fullScreenAuxiliary = 1 << 8  — may float over another app's
///                                   fullscreen Space
///   NSStatusWindowLevel = 25      — above normal and floating windows
///   NSWindowSharingNone = 0      — invisible to screen capture (default;
///                                   demo mode flips it, see above)
#[cfg(target_os = "macos")]
fn apply_native_overlay_behavior(window: &tauri::WebviewWindow) {
    let w = window.clone();
    let _ = window.run_on_main_thread(move || {
        use objc2::msg_send;
        use objc2::runtime::AnyObject;

        let Ok(raw) = w.ns_window() else { return };
        let ns_window = raw as *mut AnyObject;
        unsafe {
            let behavior: usize = msg_send![ns_window, collectionBehavior];
            let with_spaces = behavior | (1 << 0) | (1 << 8);
            let _: () = msg_send![ns_window, setCollectionBehavior: with_spaces];
            let _: () = msg_send![ns_window, setLevel: 25isize];
            let _: () = msg_send![ns_window, setSharingType: 0usize];
        }
    });
}

#[cfg(not(target_os = "macos"))]
fn apply_native_overlay_behavior(_window: &tauri::WebviewWindow) {}

/// Per-region click-through. macOS's `ignoresMouseEvents` is
/// all-or-nothing for a window, and once it's on, the webview receives no
/// mouse events — so it can never see the hover that should turn
/// interactivity back on. The way out (same as Electron overlay apps) is
/// to hit-test from outside the window: poll the global cursor position
/// and flip `ignoresMouseEvents` based on whether it's inside any
/// frontend-reported interactive region.
fn spawn_cursor_poller(app: tauri::AppHandle, session: Arc<AtomicBool>) {
    std::thread::Builder::new()
        .name("overlay-cursor-poller".into())
        .spawn(move || {
            let mut currently_ignoring: Option<bool> = None;
            while session.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(50));
                let Some(window) = app.get_webview_window(OVERLAY_LABEL) else {
                    break;
                };
                let (Ok(cursor), Ok(origin), Ok(scale)) = (
                    app.cursor_position(),
                    window.outer_position(),
                    window.scale_factor(),
                ) else {
                    continue;
                };

                // Cursor and window origin are physical pixels in the same
                // screen space; frontend rects are logical and
                // window-relative — convert before hit-testing.
                let local_x = (cursor.x - origin.x as f64) / scale;
                let local_y = (cursor.y - origin.y as f64) / scale;

                let inside_interactive = {
                    let state = app.state::<OverlayState>();
                    let regions = state.regions.lock().unwrap();
                    regions.iter().any(|r| {
                        local_x >= r.x
                            && local_x < r.x + r.width
                            && local_y >= r.y
                            && local_y < r.y + r.height
                    })
                };

                let ignore = !inside_interactive;
                if currently_ignoring != Some(ignore) {
                    let _ = window.set_ignore_cursor_events(ignore);
                    currently_ignoring = Some(ignore);
                }
            }
        })
        .expect("failed to spawn overlay-cursor-poller thread");
}

#[derive(Clone, serde::Serialize)]
struct SuggestionEvent {
    id: String,
    /// Which stream the trigger cue came from: "you" (mic) or "them"
    /// (system output) — the two-stream separation, surfaced in the UI.
    source: &'static str,
    cue: &'static str,
    hint: &'static str,
    /// Tier-2 guidance. Mock-only shortcut: shipped with the suggestion
    /// and "streamed" client-side. The real Tier-2 agent will stream this
    /// on demand after a click instead.
    detail: &'static str,
}

/// Stand-in for the Tier-1 cue agent: emits a canned suggestion every few
/// seconds on the same event the real agent will use. Delete this (and
/// nothing else) when the agent phase lands.
fn spawn_mock_suggestions(app: tauri::AppHandle, session: Arc<AtomicBool>) {
    const POOL: &[(&str, &str, &str, &str)] = &[
        (
            "them",
            "budget approval",
            "Ask who signs off on this purchase.",
            "They mentioned budget approval without naming an owner — that's usually a soft stall. Naming the approver now surfaces blockers before they cost you the next two weeks.",
        ),
        (
            "them",
            "we tried something similar before",
            "Dig into why the last attempt failed.",
            "A past failure is your best qualification tool. Ask what specifically didn't work and who felt the pain — then position against that, not a generic competitor.",
        ),
        (
            "you",
            "let me jump ahead",
            "Go back — slide 4 raised a question.",
            "You moved past the pricing slide while they were still reading it. Return to it and ask what caught their eye — unasked questions become objections later.",
        ),
        (
            "them",
            "timeline",
            "Anchor a concrete next step.",
            "Vague timelines stall deals. Propose a specific date for the follow-up and name what each side brings to it.",
        ),
        (
            "you",
            "pricing depends",
            "Give the range now — hedging reads as evasive.",
            "You deflected the pricing question. Buyers anchor on trust before numbers; a wide-but-honest range keeps the conversation open, while \"it depends\" closes it.",
        ),
    ];

    std::thread::Builder::new()
        .name("overlay-mock-suggestions".into())
        .spawn(move || {
            // First suggestion arrives quickly so the person verifying
            // isn't staring at an idle pill wondering if it's broken.
            std::thread::sleep(Duration::from_millis(2500));
            let mut i = 0usize;
            while session.load(Ordering::SeqCst) {
                if app.get_webview_window(OVERLAY_LABEL).is_none() {
                    break;
                }
                let (source, cue, hint, detail) = POOL[i % POOL.len()];
                let _ = app.emit_to(
                    OVERLAY_LABEL,
                    "overlay://suggestion",
                    SuggestionEvent {
                        id: format!("mock-{i}"),
                        source,
                        cue,
                        hint,
                        detail,
                    },
                );
                i += 1;
                std::thread::sleep(Duration::from_secs(12));
            }
        })
        .expect("failed to spawn overlay-mock-suggestions thread");
}
