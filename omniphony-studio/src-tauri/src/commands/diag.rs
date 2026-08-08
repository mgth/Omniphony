//! Diagnostics & metering controls: publication rates, the diag publication
//! toggle, and the speaker gain-table subscription handshake.
//!
//! Each command forwards a value to the renderer over OSC.

use crate::osc_listener::OscControlMsg;
use crate::{send_control, SharedState};
use tauri::State;

#[tauri::command]
pub fn control_metering_rate_hz(state: State<SharedState>, value: f32) {
    let clamped = value.max(1.0).min(1000.0);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/metering/rate_hz".to_string(),
            value: clamped,
        },
    );
}

#[tauri::command]
pub fn control_diag_rate_hz(state: State<SharedState>, value: f32) {
    let clamped = value.max(1.0).min(1000.0);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/diag/rate_hz".to_string(),
            value: clamped,
        },
    );
}

#[tauri::command]
pub fn control_diag_publication_enabled(state: State<SharedState>, enable: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/diag/enabled".to_string(),
            value: if enable != 0 { 1 } else { 0 },
        },
    );
}

/// Subscribe to one speaker's per-band gain field. `have_version` is the version
/// already cached on this client (0 if none); `speaker_index` is the speaker to
/// display. The renderer pushes that speaker's field only if the version differs,
/// then on every topology rebuild while subscribed. Sent on first consumer, on
/// speaker change, and as a 5 s heartbeat (idempotent, self-healing).
#[tauri::command]
pub fn subscribe_speaker_gaintable(
    state: State<SharedState>,
    have_version: i32,
    speaker_index: i32,
) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendArgs {
            address: "/omniphony/control/debug/speaker_gaintable/subscribe".to_string(),
            args: vec![
                rosc::OscType::Int(have_version.max(0)),
                // Not clamped: -1 (GLOBAL_ENERGY_INDEX) selects the
                // all-speaker energy field instead of one speaker's slice.
                rosc::OscType::Int(speaker_index),
            ],
        },
    );
}

/// Unsubscribe from the gain-table push stream (last consumer released). The
/// client keeps its cached table; a later re-subscribe negotiates by version.
#[tauri::command]
pub fn unsubscribe_speaker_gaintable(state: State<SharedState>) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendNoArgs {
            address: "/omniphony/control/debug/speaker_gaintable/unsubscribe".to_string(),
        },
    );
}

// ── memory diagnostics ─────────────────────────────────────────────────────
// Backend for the opt-in memory sampler (src/debug-memory.js). Inert unless
// invoked; kept in-tree so future memory hunts don't have to rebuild the
// tooling that localised the 2026-06 WebView2 growth.

/// Diagnostic: current resident/virtual memory of the Studio host process plus,
/// on Windows, the summed working set of the whole process tree (host + every
/// WebView2 child). Cross-platform anchor for memory hunts.
///
/// `rssBytes`/`virtualBytes` are the HOST process only — on Windows the actual
/// web content runs in separate `msedgewebview2.exe` children, so a WebView2
/// leak is invisible there; `treeRssBytes` captures it. (On Linux the WebKit
/// web process is also separate; `treeRssBytes` is None there because the
/// enumeration is Windows-only, but `performance.memory` is absent under
/// WebKitGTK anyway, so Linux runs rely on the WebGL/state counters.)
#[tauri::command]
pub fn debug_memory_stats() -> serde_json::Value {
    let (rss, virt) = match memory_stats::memory_stats() {
        Some(stats) => (
            Some(stats.physical_mem as u64),
            Some(stats.virtual_mem as u64),
        ),
        None => (None, None),
    };
    serde_json::json!({
        "rssBytes": rss,
        "virtualBytes": virt,
        "treeRssBytes": process_tree_rss_bytes(),
    })
}

/// Diagnostic: live sizes of every per-id map in `AppState`. If any of these
/// climbs without bound during playback, the leak is Rust-side (ids never
/// recycled); if they stay flat while host RSS climbs, the leak is in the
/// WebView/emit layer instead.
#[tauri::command]
pub fn debug_state_sizes(state: State<SharedState>) -> serde_json::Value {
    let s = state.inner.lock().unwrap();
    serde_json::json!({
        "sources": s.sources.len(),
        "sourceLevels": s.source_levels.len(),
        "speakerLevels": s.speaker_levels.len(),
        "objectSpeakerGains": s.object_speaker_gains.len(),
        "objectBandGains": s.object_band_gains.len(),
        "speakerGains": s.speaker_gains.len(),
        "objectMutes": s.object_mutes.len(),
        "speakerMutes": s.speaker_mutes.len(),
        "layouts": s.layouts.len(),
    })
}

/// Diagnostic: persist the memory-sampler CSV to a file so a clean capture can
/// be taken with DevTools CLOSED (DevTools itself runs inside WebView2 and
/// inflates the very process tree we are measuring). Overwrites a fixed file in
/// the user's Downloads dir each call; returns the path.
#[tauri::command]
pub fn debug_write_memory_csv(app: tauri::AppHandle, csv: String) -> Result<String, String> {
    use tauri::Manager;
    let dir = app
        .path()
        .download_dir()
        .or_else(|_| app.path().app_log_dir())
        .map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join("omniphony-memory.csv");
    std::fs::write(&path, csv).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().into_owned())
}

#[cfg(not(windows))]
fn process_tree_rss_bytes() -> Option<u64> {
    None
}

/// Sum the working-set size of this process and all of its descendants. On
/// Windows the WebView2 renderer/GPU/utility processes are children of the
/// Studio host, so this is where a WebView2-side leak actually shows up.
#[cfg(windows)]
fn process_tree_rss_bytes() -> Option<u64> {
    use std::collections::HashSet;
    use windows_sys::Win32::Foundation::{CloseHandle, FALSE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcessId, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return None;
        }

        // (pid, parent_pid) for every process, so we can walk our subtree.
        let mut pairs: Vec<(u32, u32)> = Vec::new();
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                pairs.push((entry.th32ProcessID, entry.th32ParentProcessID));
                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);

        // Transitive closure of descendants of the current process.
        let mut tree: HashSet<u32> = HashSet::new();
        tree.insert(GetCurrentProcessId());
        let mut changed = true;
        while changed {
            changed = false;
            for &(pid, parent) in &pairs {
                if pid != 0 && tree.contains(&parent) && tree.insert(pid) {
                    changed = true;
                }
            }
        }

        let mut total: u64 = 0;
        for &pid in &tree {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid);
            if handle.is_null() {
                continue;
            }
            let mut counters: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
            counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
            if K32GetProcessMemoryInfo(handle, &mut counters, counters.cb) != 0 {
                total += counters.WorkingSetSize as u64;
            }
            CloseHandle(handle);
        }
        Some(total)
    }
}
