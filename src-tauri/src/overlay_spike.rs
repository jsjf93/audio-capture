//! THROWAWAY SPIKE — not production code, delete once it has served.
//!
//! Exists to answer three yes/no questions about native window behavior
//! before we invest in the real suggestion-overlay UI:
//!
//!   1. Per-region click-through: can a window pass clicks through to
//!      whatever is beneath it *except* over its interactive elements?
//!   2. Fullscreen Spaces: does the window float above another app that is
//!      fullscreen in its own macOS Space (the Zoom/Meet scenario)?
//!   3. Screen-share invisibility: can the window be excluded from screen
//!      recording/sharing entirely (`NSWindowSharingNone`)?
//!
//! Nothing here touches the audio pipeline, and `AppState` doesn't know
//! this module exists.

use std::sync::Mutex;
use std::time::Duration;
use tauri::{Emitter, Manager};

pub const OVERLAY_LABEL: &str = "overlay-spike";

/// Window-relative, logical-pixel rectangles the frontend reports as
/// "interactive" (suggestion cards, the drag handle, the close button).
/// The cursor being inside any of them makes the window accept mouse
/// events; everywhere else is click-through.
#[derive(Clone, Copy, serde::Deserialize)]
pub struct Region {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Default)]
pub struct SpikeState {
    regions: Mutex<Vec<Region>>,
}

#[tauri::command]
pub fn set_overlay_interactive_regions(state: tauri::State<'_, SpikeState>, regions: Vec<Region>) {
    *state.regions.lock().unwrap() = regions;
}

#[tauri::command]
pub fn close_overlay_spike(app: tauri::AppHandle) {
    if let Some(window) = app.get_webview_window(OVERLAY_LABEL) {
        let _ = window.close();
    }
}

/// Runtime toggle for screen-share visibility. `sharingType` is an
/// ordinary mutable NSWindow property, so "hidden from screen shares" can
/// be a user setting flipped mid-session rather than a build-time
/// decision — exactly what a "demo mode" needs.
///   NSWindowSharingNone     = 0 — invisible to screen capture
///   NSWindowSharingReadOnly = 1 — capturable (normal window behavior)
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
pub fn open_overlay_spike(app: tauri::AppHandle) -> Result<(), String> {
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
    .title("Overlay Spike")
    .inner_size(340.0, 520.0)
    .position(80.0, 80.0)
    .transparent(true) // needs `macOSPrivateApi: true` in tauri.conf.json
    .decorations(false)
    .always_on_top(true)
    .visible_on_all_workspaces(true)
    .focused(false) // an overlay shouldn't yank focus away from the call
    .build()
    .map_err(|e| e.to_string())?;

    apply_native_overlay_behavior(&window);
    spawn_cursor_poller(app);
    Ok(())
}

/// The three NSWindow properties Tauri's builder has no knobs for, applied
/// via raw Objective-C messages. Must run on the main thread (AppKit rule).
///
/// Constants inlined from the AppKit headers rather than pulling in the
/// full `objc2-app-kit` bindings for three calls of throwaway code:
///   canJoinAllSpaces    = 1 << 0  (NSWindowCollectionBehavior)
///   fullScreenAuxiliary = 1 << 8  — may float over another app's
///                                   fullscreen Space
///   NSStatusWindowLevel = 25      — above normal and floating windows
///   NSWindowSharingNone = 0       — invisible to screen capture
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

/// The core trick behind per-region click-through. macOS's
/// `ignoresMouseEvents` is all-or-nothing for a window — and once it's on,
/// the window receives *no* mouse events, so the webview can never see the
/// hover that should turn interactivity back on. The way out (the same one
/// Electron overlay apps use) is to hit-test from outside the window: poll
/// the global cursor position from Rust and flip `ignoresMouseEvents`
/// depending on whether the cursor is inside a frontend-reported
/// interactive region.
fn spawn_cursor_poller(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        let mut currently_ignoring: Option<bool> = None;
        loop {
            std::thread::sleep(Duration::from_millis(50));
            let Some(window) = app.get_webview_window(OVERLAY_LABEL) else {
                break; // window closed — the poller dies with it
            };
            let (Ok(cursor), Ok(origin), Ok(scale)) = (
                app.cursor_position(),
                window.outer_position(),
                window.scale_factor(),
            ) else {
                continue;
            };

            // Cursor and window origin are physical pixels in the same
            // screen space; the frontend's rects are logical and
            // window-relative, so convert before hit-testing.
            let local_x = (cursor.x - origin.x as f64) / scale;
            let local_y = (cursor.y - origin.y as f64) / scale;

            let inside_interactive = {
                let state = app.state::<SpikeState>();
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
                // Purely so the person verifying can watch the mode flip.
                let _ = app.emit_to(OVERLAY_LABEL, "overlay-spike://passthrough", ignore);
                currently_ignoring = Some(ignore);
            }
        }
    });
}
