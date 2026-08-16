//! Scrim application shell.
//!
//! At this stage this is the compositing spike: prove that mpv's picture and
//! the HTML UI can share one window before building the rest on top of it.

#[cfg(windows)]
mod video_host;

use std::sync::Mutex;

use tauri::{Manager, State};

#[cfg(windows)]
use video_host::VideoHost;

#[derive(Default)]
struct AppState {
    #[cfg(windows)]
    host: Mutex<Option<VideoHost>>,
    mpv: Mutex<Option<std::process::Child>>,
    /// Last stage rectangle the interface reported, in physical pixels
    /// relative to the UI window's client area. The video window lives in
    /// screen coordinates, so it has to be re-placed whenever the UI window
    /// moves, and a move produces no layout change for the interface to
    /// notice. Keeping the last rectangle lets Rust re-apply it on its own.
    last_bounds: Mutex<Option<(i32, i32, i32, i32)>>,
}

/// Where the UI wants the picture, in CSS pixels relative to the window.
#[derive(serde::Deserialize)]
struct Bounds {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    /// devicePixelRatio, so a high-DPI display lines up with Win32's pixels.
    scale: f64,
}

#[tauri::command]
fn set_stage_bounds(state: State<'_, AppState>, bounds: Bounds) {
    #[cfg(windows)]
    {
        let rect = (
            (bounds.x * bounds.scale).round() as i32,
            (bounds.y * bounds.scale).round() as i32,
            (bounds.width * bounds.scale).round() as i32,
            (bounds.height * bounds.scale).round() as i32,
        );
        *state.last_bounds.lock().unwrap() = Some(rect);
        if let Some(host) = state.host.lock().unwrap().as_ref() {
            host.set_bounds(rect.0, rect.1, rect.2, rect.3);
        }
    }
    #[cfg(not(windows))]
    let _ = (state, bounds);
}

/// Re-place the video window from the remembered rectangle.
#[cfg(windows)]
fn reapply_bounds(state: &AppState) {
    let rect = *state.last_bounds.lock().unwrap();
    if let Some(host) = state.host.lock().unwrap().as_ref() {
        match rect {
            Some((x, y, w, h)) => host.set_bounds(x, y, w, h),
            None => host.fill_parent(),
        }
    }
}

/// Spike command: start mpv inside the stage window with a filtergraph.
///
/// The real player will drive mpv over a named pipe instead of respawning it,
/// but for proving compositing, spawning is enough.
#[tauri::command]
fn spike_play(state: State<'_, AppState>, path: String, graph: String) -> Result<String, String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;

        let wid = {
            let guard = state.host.lock().unwrap();
            let host = guard.as_ref().ok_or("video stage was never created")?;
            host.sink_to_bottom();
            host.wid()
        };

        // Stop anything already playing: one pipeline at a time.
        if let Some(mut old) = state.mpv.lock().unwrap().take() {
            let _ = old.kill();
        }

        let mut cmd = std::process::Command::new("mpv");
        cmd.arg(&path)
            .arg(format!("--wid={wid}"))
            .arg("--hwdec=no")
            .arg("--keep-open=yes")
            .arg("--osc=no")
            .arg("--input-default-bindings=no")
            .arg("--msg-level=all=error")
            .arg("--force-seekable=yes")
            // SPIKE ONLY: jump straight to a span abc.mp4 actually covers, so
            // a screenshot proves the censor box and the HTML controls are
            // compositing together rather than showing a clean frame.
            .arg("--start=858")
            .creation_flags(CREATE_NO_WINDOW);

        if !graph.is_empty() {
            // A conf file sidesteps the Windows command line length limit,
            // which a 56,000 character filtergraph blows straight past.
            let conf = std::env::temp_dir().join("scrim-spike.conf");
            std::fs::write(&conf, format!("vf=lavfi=[{graph}]\nhwdec=no\n"))
                .map_err(|e| format!("writing mpv conf: {e}"))?;
            cmd.arg(format!("--include={}", conf.display()));
        }

        let child = cmd
            .spawn()
            .map_err(|e| format!("could not start mpv ({e}). Is it on PATH?"))?;
        *state.mpv.lock().unwrap() = Some(child);

        Ok(format!("mpv started on wid {wid}"))
    }
    #[cfg(not(windows))]
    {
        let _ = (state, path, graph);
        Err("Scrim is Windows-only for now".into())
    }
}

/// Build a filtergraph from a plan file, so the spike shows real censoring
/// rather than a clean picture.
#[tauri::command]
fn graph_for_plan(plan_path: String, style: String) -> Result<String, String> {
    use scrim_core::{CensorStyle, Coverage, Plan, WindowParams};

    let text = std::fs::read_to_string(&plan_path).map_err(|e| format!("reading plan: {e}"))?;
    let plan: Plan = serde_json::from_str(&text).map_err(|e| format!("parsing plan: {e}"))?;
    plan.validate().map_err(|e| e.to_string())?;

    let style = match style.as_str() {
        "white_box" => CensorStyle::WhiteBox,
        "blur_strong" => CensorStyle::BlurStrong,
        "blur_medium" => CensorStyle::BlurMedium,
        "blur_light" => CensorStyle::BlurLight,
        _ => CensorStyle::BlackBox,
    };

    let coverage = Coverage::from_plan(&plan, &WindowParams::default());
    Ok(coverage.graph(plan.source.width, plan.source.height, style))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            set_stage_bounds,
            spike_play,
            graph_for_plan
        ])
        .setup(|app| {
            #[cfg(windows)]
            {
                use windows::Win32::Foundation::HWND;

                let window = app.get_webview_window("main").expect("main window");
                let hwnd = HWND(window.hwnd()?.0 as *mut _);

                // Diagnostic: skip the video window entirely. If the desktop is
                // visible through the stage with this set, the webview really is
                // transparent and the problem is sibling child compositing.
                if std::env::var("SCRIM_NO_STAGE").as_deref() == Ok("1") {
                    return Ok(());
                }

                let host = VideoHost::new(hwnd).map_err(|e| -> Box<dyn std::error::Error> {
                    format!("video stage: {e}").into()
                })?;
                host.fill_parent();

                let state = app.state::<AppState>();
                *state.host.lock().unwrap() = Some(host);
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            match event {
                // The video window lives in screen coordinates, so it has to
                // follow the interface on both moves and resizes.
                tauri::WindowEvent::Moved(_) | tauri::WindowEvent::Resized(_) => {
                    #[cfg(windows)]
                    reapply_bounds(&window.state::<AppState>());
                }
                tauri::WindowEvent::CloseRequested { .. } | tauri::WindowEvent::Destroyed => {
                    let state = window.state::<AppState>();
                    if let Some(mut child) = state.mpv.lock().unwrap().take() {
                        let _ = child.kill();
                    }
                    // Drop the video window before the interface goes, so the
                    // ownership link is unhooked in the right order.
                    #[cfg(windows)]
                    drop(state.host.lock().unwrap().take());
                }
                _ => {}
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running Scrim");
}
