//! Scrim: the application.
//!
//! This module holds the state machine and the commands the interface calls.
//! The parts that decide what gets covered live in `scrim-core`; the parts
//! that decide what is *safe* live here, in the fail-closed rules:
//!
//!   * scanned-plan mode refuses to play without a complete, valid plan;
//!   * live mode fences playback behind the detection frontier;
//!   * casting refuses anything but a complete plan.

#[cfg(windows)]
mod video_host;

mod fence;
mod paths;
mod store;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use scrim_cast::{CastConfig, CastSession, Device};
use scrim_core::{CensorStyle, Coverage, Plan, WindowParams};
use scrim_detect::{Scan, ScanConfig};
use scrim_mpv::{Mpv, MpvEvent, MpvOptions};

use fence::{FenceAction, FenceInput};
use paths::Paths;
use store::{plan_path, Settings, Store};

#[cfg(windows)]
use video_host::VideoHost;

/// How often live windows are pushed into a running mpv.
const GRAPH_PUSH_INTERVAL: Duration = Duration::from_secs(20);

// ============================================================== state =====

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Mode {
    ScannedPlan,
    Live,
}

#[derive(Default)]
struct Playback {
    file: Option<PathBuf>,
    mode: Option<Mode>,
    position: f64,
    duration: f64,
    paused: bool,
    coverage: Option<Coverage>,
    frame: (i64, i64),
    paused_by_fence: bool,
    /// Live mode waits this long before starting playback.
    head_start_until: Option<Instant>,
    started: bool,
    last_push: Option<Instant>,
    last_window_count: usize,
}

pub struct App {
    paths: Paths,
    store: Mutex<Store>,
    playback: Mutex<Playback>,
    mpv: tokio::sync::Mutex<Option<Mpv>>,
    scans: Mutex<HashMap<PathBuf, Arc<Scan>>>,
    cast: Mutex<Option<CastSession>>,
    cast_devices: Mutex<Vec<Device>>,
    #[cfg(windows)]
    host: Mutex<Option<VideoHost>>,
    last_bounds: Mutex<Option<(i32, i32, i32, i32)>>,
}

impl App {
    fn style(&self) -> CensorStyle {
        parse_style(&self.store.lock().unwrap().settings.censor_style)
    }

    fn window_params(&self) -> WindowParams {
        let s = &self.store.lock().unwrap().settings;
        WindowParams {
            pad_before: s.lead_before,
            pad_after: s.hold_after,
            margin: s.margin,
            ..WindowParams::default()
        }
    }
}

fn parse_style(s: &str) -> CensorStyle {
    match s {
        "white_box" => CensorStyle::WhiteBox,
        "blur_strong" => CensorStyle::BlurStrong,
        "blur_medium" => CensorStyle::BlurMedium,
        "blur_light" => CensorStyle::BlurLight,
        _ => CensorStyle::BlackBox,
    }
}

// ============================================================ payloads ====

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct MovieInfo {
    path: String,
    name: String,
    /// "ready", "scanning", "unscanned" or "unreadable"
    status: String,
    duration: f64,
    windows: usize,
    covered_seconds: f64,
    scan_percent: f64,
    scan_speed: f64,
    subtitle: Option<String>,
    sub_delay: f64,
    resume_at: f64,
    error: Option<String>,
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
struct PlaybackState {
    file: Option<String>,
    name: Option<String>,
    mode: Option<Mode>,
    position: f64,
    duration: f64,
    paused: bool,
    playing: bool,
    windows: Vec<(f64, f64)>,
    covered_seconds: f64,
    censor_style: String,
    volume: i64,
    subtitle: Option<String>,
    sub_delay: f64,
    // live mode
    live: bool,
    frontier: f64,
    scan_complete: bool,
    scan_speed: f64,
    detections: usize,
    head_start_remaining: f64,
    // fence
    fenced: bool,
    fence_resume_in: Option<f64>,
    fence_reason: Option<String>,
    casting: Option<String>,
}

// ============================================================ commands ====

#[derive(serde::Deserialize)]
struct Bounds {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    scale: f64,
}

#[tauri::command]
fn set_stage_bounds(app: State<'_, App>, bounds: Bounds) {
    #[cfg(windows)]
    {
        let rect = (
            (bounds.x * bounds.scale).round() as i32,
            (bounds.y * bounds.scale).round() as i32,
            (bounds.width * bounds.scale).round() as i32,
            (bounds.height * bounds.scale).round() as i32,
        );
        *app.last_bounds.lock().unwrap() = Some(rect);
        if let Some(host) = app.host.lock().unwrap().as_ref() {
            host.set_bounds(rect.0, rect.1, rect.2, rect.3);
        }
    }
    #[cfg(not(windows))]
    let _ = (app, bounds);
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AppInfo {
    version: String,
    missing: Vec<paths::MissingResource>,
    settings: Settings,
}

#[tauri::command]
fn app_info(app: State<'_, App>) -> AppInfo {
    AppInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        missing: app.paths.missing(),
        settings: app.store.lock().unwrap().settings.clone(),
    }
}

#[tauri::command]
fn get_settings(app: State<'_, App>) -> Settings {
    app.store.lock().unwrap().settings.clone()
}

#[tauri::command]
async fn set_settings(app: State<'_, App>, settings: Settings) -> Result<(), String> {
    {
        let mut store = app.store.lock().unwrap();
        store.settings = settings;
        store.save();
    }
    // The censor style and volume apply to a running movie immediately.
    let style = app.style();
    let volume = app.store.lock().unwrap().settings.volume;
    if let Some(mpv) = app.mpv.lock().await.as_ref() {
        mpv.set_volume(volume);
        let pb = app.playback.lock().unwrap();
        if let Some(cov) = &pb.coverage {
            mpv.set_filtergraph(&cov.graph(pb.frame.0, pb.frame.1, style));
        }
    }
    Ok(())
}

#[tauri::command]
fn library_list(app: State<'_, App>) -> Vec<MovieInfo> {
    let store = app.store.lock().unwrap();
    let scans = app.scans.lock().unwrap();
    let params = WindowParams {
        pad_before: store.settings.lead_before,
        pad_after: store.settings.hold_after,
        margin: store.settings.margin,
        ..WindowParams::default()
    };

    store
        .library
        .movies
        .iter()
        .map(|path| {
            let state = store.state_of(path);
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();

            let mut info = MovieInfo {
                path: path.to_string_lossy().into_owned(),
                name,
                status: "unscanned".into(),
                duration: 0.0,
                windows: 0,
                covered_seconds: 0.0,
                scan_percent: 0.0,
                scan_speed: 0.0,
                subtitle: state.subtitle.map(|p| p.to_string_lossy().into_owned()),
                sub_delay: state.sub_delay,
                resume_at: state.position,
                error: None,
            };

            if let Some(scan) = scans.get(path) {
                let p = scan.progress();
                info.status = "scanning".into();
                info.duration = p.duration;
                info.scan_percent = if p.duration > 0.0 {
                    100.0 * p.frontier / p.duration
                } else {
                    0.0
                };
                info.scan_speed = p.speed;
                info.error = p.error;
                return info;
            }

            match load_plan(path) {
                Ok(plan) => {
                    let cov = Coverage::from_plan(&plan, &params);
                    info.status = if plan.complete { "ready" } else { "unscanned" }.into();
                    info.duration = plan.source.duration;
                    info.windows = cov.windows.len();
                    info.covered_seconds = cov.covered_seconds();
                }
                Err(e) if e.is_some() => {
                    info.status = "unreadable".into();
                    info.error = e;
                }
                Err(_) => {}
            }
            info
        })
        .collect()
}

/// `Ok(plan)`, `Err(None)` when there is simply no plan, `Err(Some(why))` when
/// there is one and it cannot be trusted.
fn load_plan(video: &Path) -> Result<Plan, Option<String>> {
    let path = plan_path(video);
    if !path.exists() {
        return Err(None);
    }
    let text = std::fs::read_to_string(&path).map_err(|e| Some(e.to_string()))?;
    let plan: Plan = serde_json::from_str(&text).map_err(|e| Some(e.to_string()))?;
    plan.validate().map_err(|e| Some(e.to_string()))?;
    Ok(plan)
}

#[tauri::command]
fn library_add(app: State<'_, App>, paths: Vec<PathBuf>) -> usize {
    let mut store = app.store.lock().unwrap();
    let added = store.add(paths);
    store.save();
    added
}

#[tauri::command]
fn library_remove(app: State<'_, App>, path: PathBuf) {
    let mut store = app.store.lock().unwrap();
    store.remove(&path);
    store.save();
}

// ---------------------------------------------------------------- scanning

#[tauri::command]
fn scan_start(app: State<'_, App>, path: PathBuf) -> Result<(), String> {
    if app.scans.lock().unwrap().contains_key(&path) {
        return Err("this movie is already being scanned".into());
    }
    // Report everything that is absent, not just the first thing found. Fixing
    // one missing file only to be told about the next is a miserable loop to
    // be stuck in.
    let missing = app.paths.missing();
    if !missing.is_empty() {
        let names: Vec<_> = missing.iter().map(|m| m.name.as_str()).collect();
        return Err(format!(
            "Scrim cannot scan without {}. Expected in {}.",
            names.join(", "),
            missing[0]
                .expected_at
                .rsplit_once('\\')
                .map(|(dir, _)| dir)
                .unwrap_or(&missing[0].expected_at)
        ));
    }

    let scan = Scan::start(
        &path,
        ScanConfig {
            ffmpeg: app.paths.ffmpeg.clone(),
            model: app.paths.model.clone(),
            onnxruntime: Some(app.paths.onnxruntime.clone()),
        },
    )?;
    app.scans.lock().unwrap().insert(path, Arc::new(scan));
    Ok(())
}

#[tauri::command]
fn scan_stop(app: State<'_, App>, path: PathBuf) {
    if let Some(scan) = app.scans.lock().unwrap().remove(&path) {
        scan.stop();
    }
}

// ---------------------------------------------------------------- playback

#[tauri::command]
async fn play(
    app: State<'_, App>,
    handle: AppHandle,
    path: PathBuf,
    mode: String,
) -> Result<(), String> {
    let live = mode == "live";
    stop_playback(&app).await;

    if !path.exists() {
        return Err("that file is no longer there".into());
    }

    let params = app.window_params();
    let style = app.style();

    let (coverage, frame, duration) = if live {
        // Reuse a scan already in flight for this movie, otherwise start one.
        let existing = app.scans.lock().unwrap().get(&path).cloned();
        let scan = match existing {
            Some(s) => s,
            None => {
                scan_start(app.clone(), path.clone())?;
                app.scans
                    .lock()
                    .unwrap()
                    .get(&path)
                    .cloned()
                    .ok_or("the scan did not start")?
            }
        };
        let info = scan.info().clone();
        let plan = scan.plan();
        (
            Coverage::from_plan(&plan, &params),
            (info.width, info.height),
            info.duration,
        )
    } else {
        // Fail closed: scanned-plan mode will not play without a plan it can
        // read and that covers the whole movie.
        let plan = load_plan(&path).map_err(|e| {
            e.unwrap_or_else(|| {
                "this movie has not been scanned yet. Scan it, or play it with live detection."
                    .into()
            })
        })?;
        if !plan.complete {
            return Err(
                "the scan of this movie never finished, so parts of it were never looked at. \
                 Scan it again, or play it with live detection."
                    .into(),
            );
        }
        (
            Coverage::from_plan(&plan, &params),
            (plan.source.width, plan.source.height),
            plan.source.duration,
        )
    };

    let head_start = {
        let store = app.store.lock().unwrap();
        Duration::from_secs(store.settings.head_start_minutes.clamp(1, 10) as u64 * 60)
    };

    {
        let mut pb = app.playback.lock().unwrap();
        *pb = Playback {
            file: Some(path.clone()),
            mode: Some(if live { Mode::Live } else { Mode::ScannedPlan }),
            duration,
            coverage: Some(coverage),
            frame,
            head_start_until: live.then(|| Instant::now() + head_start),
            started: !live,
            ..Default::default()
        };
    }

    if !live {
        start_mpv(&app, &handle, &path, style).await?;
    }
    Ok(())
}

async fn start_mpv(
    app: &State<'_, App>,
    handle: &AppHandle,
    path: &Path,
    style: CensorStyle,
) -> Result<(), String> {
    let (graph, frame) = {
        let pb = app.playback.lock().unwrap();
        let cov = pb.coverage.clone().unwrap_or_else(|| Coverage {
            windows: Vec::new(),
            full_runs: Vec::new(),
        });
        (cov.graph(pb.frame.0, pb.frame.1, style), pb.frame)
    };
    let _ = frame;

    let conf = if graph.is_empty() {
        None
    } else {
        let conf = app.paths.playback_conf();
        std::fs::write(&conf, format!("vf=lavfi=[{graph}]\nhwdec=no\n"))
            .map_err(|e| format!("could not write the playback config: {e}"))?;
        Some(conf)
    };

    let (state, volume) = {
        let store = app.store.lock().unwrap();
        (store.state_of(path), store.settings.volume)
    };

    let wid = {
        #[cfg(windows)]
        {
            let guard = app.host.lock().unwrap();
            let host = guard.as_ref().ok_or("the video window is missing")?;
            host.show();
            host.wid()
        }
        #[cfg(not(windows))]
        0
    };

    let (mpv, mut events) = Mpv::start(MpvOptions {
        exe: app.paths.mpv.clone(),
        video: path.to_path_buf(),
        wid,
        conf,
        start: (state.position > 5.0).then_some(state.position),
        volume,
        subtitle: state.subtitle.clone(),
        sub_delay: state.sub_delay,
        paused: false,
    })
    .await
    .map_err(|e| e.to_string())?;

    *app.mpv.lock().await = Some(mpv);

    // Mirror mpv's property changes into our own state.
    let app_handle = handle.clone();
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            let state = app_handle.state::<App>();
            let mut pb = state.playback.lock().unwrap();
            match event {
                MpvEvent::TimePos { seconds } => pb.position = seconds,
                MpvEvent::Duration { seconds } => {
                    if seconds > 0.0 {
                        pb.duration = seconds
                    }
                }
                MpvEvent::Paused { paused } => pb.paused = paused,
                MpvEvent::EndOfFile => {}
                MpvEvent::Exited => {
                    pb.file = None;
                    pb.mode = None;
                    break;
                }
            }
        }
    });

    Ok(())
}

async fn stop_playback(app: &State<'_, App>) {
    if let Some(mut mpv) = app.mpv.lock().await.take() {
        mpv.stop().await;
    }
    #[cfg(windows)]
    if let Some(host) = app.host.lock().unwrap().as_ref() {
        host.hide();
    }
    let mut pb = app.playback.lock().unwrap();
    // Remember where they were, so a long film can be picked up again.
    if let (Some(file), true) = (pb.file.clone(), pb.position > 10.0) {
        let mut store = app.store.lock().unwrap();
        let pos = pb.position;
        store.update_state(&file, |s| s.position = pos);
        store.save();
    }
    *pb = Playback::default();
}

#[tauri::command]
async fn stop(app: State<'_, App>) -> Result<(), String> {
    stop_playback(&app).await;
    Ok(())
}

#[tauri::command]
async fn toggle_pause(app: State<'_, App>) -> Result<(), String> {
    // A fence pause is not the viewer's to undo; it lifts on its own.
    if app.playback.lock().unwrap().paused_by_fence {
        return Ok(());
    }
    if let Some(mpv) = app.mpv.lock().await.as_ref() {
        mpv.toggle_pause();
    }
    Ok(())
}

#[tauri::command]
async fn seek(app: State<'_, App>, seconds: f64, exact: bool) -> Result<(), String> {
    // In live mode a seek past the frontier is refused before it happens,
    // rather than corrected after the fact.
    let target = {
        let pb = app.playback.lock().unwrap();
        if pb.mode == Some(Mode::Live) {
            let (frontier, complete) = frontier_of(&app, pb.file.as_deref());
            if !complete && seconds > frontier - 5.0 {
                (frontier - 30.0).max(0.0)
            } else {
                seconds
            }
        } else {
            seconds
        }
    };

    if let Some(mpv) = app.mpv.lock().await.as_ref() {
        if exact {
            mpv.seek_exact(target);
        } else {
            mpv.seek_scrub(target);
        }
    }
    Ok(())
}

#[tauri::command]
async fn set_volume(app: State<'_, App>, volume: i64) -> Result<(), String> {
    {
        let mut store = app.store.lock().unwrap();
        store.settings.volume = volume.clamp(0, 130);
        store.save();
    }
    if let Some(mpv) = app.mpv.lock().await.as_ref() {
        mpv.set_volume(volume);
    }
    Ok(())
}

#[tauri::command]
async fn set_censor_style(app: State<'_, App>, style: String) -> Result<(), String> {
    {
        let mut store = app.store.lock().unwrap();
        store.settings.censor_style = style.clone();
        store.save();
    }
    let style = parse_style(&style);
    let graph = {
        let pb = app.playback.lock().unwrap();
        pb.coverage
            .as_ref()
            .map(|c| c.graph(pb.frame.0, pb.frame.1, style))
    };
    if let (Some(mpv), Some(graph)) = (app.mpv.lock().await.as_ref(), graph) {
        // Rebuilt and applied over IPC: no restart, no rescan.
        mpv.set_filtergraph(&graph);
    }
    Ok(())
}

#[tauri::command]
async fn set_subtitle(app: State<'_, App>, path: Option<PathBuf>) -> Result<(), String> {
    let file = app.playback.lock().unwrap().file.clone();
    let Some(file) = file else {
        return Err("nothing is playing".into());
    };
    {
        let mut store = app.store.lock().unwrap();
        store.update_state(&file, |s| s.subtitle = path.clone());
        store.save();
    }
    if let (Some(mpv), Some(p)) = (app.mpv.lock().await.as_ref(), path) {
        mpv.add_subtitle(&p.to_string_lossy());
    }
    Ok(())
}

#[tauri::command]
async fn adjust_sub_delay(app: State<'_, App>, delta: f64) -> Result<f64, String> {
    let file = app.playback.lock().unwrap().file.clone();
    let Some(file) = file else {
        return Err("nothing is playing".into());
    };
    let delay = {
        let mut store = app.store.lock().unwrap();
        let mut out = 0.0;
        store.update_state(&file, |s| {
            s.sub_delay = ((s.sub_delay + delta) * 100.0).round() / 100.0;
            out = s.sub_delay;
        });
        store.save();
        out
    };
    if let Some(mpv) = app.mpv.lock().await.as_ref() {
        mpv.set_sub_delay(delay);
    }
    Ok(delay)
}

// ------------------------------------------------------------------ cast

#[tauri::command]
async fn cast_discover(app: State<'_, App>) -> Result<Vec<Device>, String> {
    let devices = tokio::task::spawn_blocking(|| scrim_cast::discover(Duration::from_secs(5)))
        .await
        .map_err(|e| e.to_string())??;
    *app.cast_devices.lock().unwrap() = devices.clone();
    Ok(devices)
}

#[tauri::command]
async fn cast_start(app: State<'_, App>, host: String) -> Result<String, String> {
    let device = app
        .cast_devices
        .lock()
        .unwrap()
        .iter()
        .find(|d| d.host == host)
        .cloned()
        .ok_or("that device is no longer on the network")?;

    let file = app
        .playback
        .lock()
        .unwrap()
        .file
        .clone()
        .ok_or("choose a movie before casting")?;

    // Fail closed: the television gets a finished plan or nothing. A live scan
    // in progress cannot be cast, because the end of the movie has not been
    // looked at and the stream cannot be fenced once it has left this machine.
    let plan = load_plan(&file)
        .map_err(|e| e.unwrap_or_else(|| "casting needs a finished scan of this movie".into()))?;
    if !plan.complete {
        return Err("casting needs a finished scan; this one never completed".into());
    }

    let start = app.playback.lock().unwrap().position;
    let params = app.window_params();
    let style = app.style();
    let coverage = Coverage::from_plan(&plan, &params);

    // Shift the timings, because ffmpeg's clock restarts at the seek point.
    let shifted: Vec<_> = coverage
        .windows
        .iter()
        .map(|w| scrim_cast::scrim_window::Window {
            start: w.start,
            end: w.end,
            x: w.x,
            y: w.y,
            w: w.w,
            h: w.h,
        })
        .collect();
    let shifted = scrim_cast::shift_windows(&shifted, start);
    let windows: Vec<scrim_core::CensorWindow> = shifted
        .iter()
        .map(|w| scrim_core::CensorWindow {
            start: w.start,
            end: w.end,
            x: w.x,
            y: w.y,
            w: w.w,
            h: w.h,
        })
        .collect();

    let graph =
        scrim_core::build_graph(&[], &windows, plan.source.width, plan.source.height, style);

    // One heavy pipeline at a time.
    stop_playback(&app).await;

    let cfg = CastConfig {
        ffmpeg: app.paths.ffmpeg.clone(),
        video: file,
        graph,
        start,
    };
    let session = tokio::task::spawn_blocking(move || CastSession::start(&device, cfg))
        .await
        .map_err(|e| e.to_string())??;

    let name = session.device_name().to_string();
    *app.cast.lock().unwrap() = Some(session);
    Ok(name)
}

#[tauri::command]
fn cast_stop(app: State<'_, App>) {
    if let Some(mut session) = app.cast.lock().unwrap().take() {
        session.stop_cast();
    }
}

// ============================================================== the tick ==

fn frontier_of(app: &State<'_, App>, file: Option<&Path>) -> (f64, bool) {
    let Some(file) = file else {
        return (0.0, true);
    };
    match app.scans.lock().unwrap().get(file) {
        Some(scan) => {
            let p = scan.progress();
            (p.frontier, p.done)
        }
        None => (f64::MAX, true),
    }
}

/// Runs four times a second: publishes state, drives live mode, applies fences.
async fn tick(handle: AppHandle) {
    let app = handle.state::<App>();

    // A finished scan becomes a saved plan, so the next viewing is instant.
    let finished: Vec<(PathBuf, Arc<Scan>)> = {
        let scans = app.scans.lock().unwrap();
        scans
            .iter()
            .filter(|(_, s)| s.progress().done)
            .map(|(p, s)| (p.clone(), s.clone()))
            .collect()
    };
    for (path, scan) in finished {
        let plan = scan.plan();
        if let Ok(text) = serde_json::to_string_pretty(&plan) {
            let _ = std::fs::write(plan_path(&path), text);
        }
        app.scans.lock().unwrap().remove(&path);
        let _ = handle.emit("library-changed", ());
    }

    let file = app.playback.lock().unwrap().file.clone();
    let Some(file) = file else {
        // Idle still carries the settings the controls display, or the volume
        // and censor picker would read as zero and blank until something plays.
        let store = app.store.lock().unwrap();
        let _ = handle.emit(
            "playback",
            PlaybackState {
                volume: store.settings.volume,
                censor_style: store.settings.censor_style.clone(),
                casting: app
                    .cast
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map(|c| c.device_name().to_string()),
                ..PlaybackState::default()
            },
        );
        return;
    };

    let live = app.playback.lock().unwrap().mode == Some(Mode::Live);
    let (frontier, complete) = if live {
        frontier_of(&app, Some(&file))
    } else {
        (f64::MAX, true)
    };

    // --- live mode: start playing once detection has a head start ---------
    if live {
        let should_start = {
            let pb = app.playback.lock().unwrap();
            !pb.started
                && (complete
                    || pb
                        .head_start_until
                        .map(|d| Instant::now() >= d)
                        .unwrap_or(true))
        };
        if should_start {
            refresh_live_coverage(&app, &file);
            app.playback.lock().unwrap().started = true;
            let style = app.style();
            if let Err(e) = start_mpv(&app, &handle, &file, style).await {
                let _ = handle.emit("error", e);
                app.playback.lock().unwrap().started = false;
            }
        }

        // --- push newly found windows into the running movie --------------
        let due = {
            let pb = app.playback.lock().unwrap();
            pb.started
                && pb
                    .last_push
                    .map(|t| t.elapsed() >= GRAPH_PUSH_INTERVAL)
                    .unwrap_or(true)
        };
        if due && !complete {
            let changed = refresh_live_coverage(&app, &file);
            if changed {
                let style = app.style();
                let graph = {
                    let pb = app.playback.lock().unwrap();
                    pb.coverage
                        .as_ref()
                        .map(|c| c.graph(pb.frame.0, pb.frame.1, style))
                };
                if let (Some(mpv), Some(graph)) = (app.mpv.lock().await.as_ref(), graph) {
                    mpv.set_filtergraph(&graph);
                }
            }
            app.playback.lock().unwrap().last_push = Some(Instant::now());
        }

        // --- the fences ---------------------------------------------------
        let input = {
            let pb = app.playback.lock().unwrap();
            FenceInput {
                position: pb.position,
                frontier,
                scan_complete: complete,
                paused_by_fence: pb.paused_by_fence,
            }
        };
        if app.playback.lock().unwrap().started {
            match fence::decide(input) {
                FenceAction::SnapBack { to } => {
                    if let Some(mpv) = app.mpv.lock().await.as_ref() {
                        mpv.seek_exact(to);
                    }
                }
                FenceAction::Pause => {
                    if let Some(mpv) = app.mpv.lock().await.as_ref() {
                        mpv.set_paused(true);
                    }
                    app.playback.lock().unwrap().paused_by_fence = true;
                }
                FenceAction::Resume => {
                    if let Some(mpv) = app.mpv.lock().await.as_ref() {
                        mpv.set_paused(false);
                    }
                    app.playback.lock().unwrap().paused_by_fence = false;
                }
                FenceAction::None => {}
            }
        }
    }

    let _ = handle.emit("playback", snapshot(&app, frontier, complete));
}

/// Rebuild coverage from a live scan's detections so far.
fn refresh_live_coverage(app: &State<'_, App>, file: &Path) -> bool {
    let Some(scan) = app.scans.lock().unwrap().get(file).cloned() else {
        return false;
    };
    let plan = scan.plan();
    let params = app.window_params();
    let coverage = Coverage::from_plan(&plan, &params);
    let count = coverage.windows.len();

    let mut pb = app.playback.lock().unwrap();
    let changed = count != pb.last_window_count;
    pb.last_window_count = count;
    pb.frame = (plan.source.width, plan.source.height);
    if pb.duration <= 0.0 {
        pb.duration = plan.source.duration;
    }
    pb.coverage = Some(coverage);
    changed
}

fn snapshot(app: &State<'_, App>, frontier: f64, complete: bool) -> PlaybackState {
    let pb = app.playback.lock().unwrap();
    let store = app.store.lock().unwrap();
    let state = pb
        .file
        .as_ref()
        .map(|f| store.state_of(f))
        .unwrap_or_default();

    let scan = pb
        .file
        .as_ref()
        .and_then(|f| app.scans.lock().unwrap().get(f).cloned());
    let progress = scan.map(|s| s.progress()).unwrap_or_default();

    let input = FenceInput {
        position: pb.position,
        frontier,
        scan_complete: complete,
        paused_by_fence: pb.paused_by_fence,
    };

    PlaybackState {
        file: pb.file.as_ref().map(|f| f.to_string_lossy().into_owned()),
        name: pb
            .file
            .as_ref()
            .and_then(|f| f.file_name())
            .map(|n| n.to_string_lossy().into_owned()),
        mode: pb.mode,
        position: pb.position,
        duration: pb.duration,
        paused: pb.paused,
        playing: pb.started && pb.file.is_some(),
        windows: pb
            .coverage
            .as_ref()
            .map(|c| c.windows.iter().map(|w| (w.start, w.end)).collect())
            .unwrap_or_default(),
        covered_seconds: pb
            .coverage
            .as_ref()
            .map(|c| c.covered_seconds())
            .unwrap_or(0.0),
        censor_style: store.settings.censor_style.clone(),
        volume: store.settings.volume,
        subtitle: state.subtitle.map(|p| p.to_string_lossy().into_owned()),
        sub_delay: state.sub_delay,
        live: pb.mode == Some(Mode::Live),
        frontier: if frontier.is_finite() {
            frontier
        } else {
            pb.duration
        },
        scan_complete: complete,
        scan_speed: progress.speed,
        detections: progress.detections,
        head_start_remaining: pb
            .head_start_until
            .map(|d| d.saturating_duration_since(Instant::now()).as_secs_f64())
            .unwrap_or(0.0),
        fenced: pb.paused_by_fence,
        fence_resume_in: fence::resume_estimate(input, progress.speed),
        fence_reason: pb
            .paused_by_fence
            .then(|| "detection has not looked past this point yet".to_string()),
        casting: app
            .cast
            .lock()
            .unwrap()
            .as_ref()
            .map(|c| c.device_name().to_string()),
    }
}

// ================================================================= setup ==

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            set_stage_bounds,
            app_info,
            get_settings,
            set_settings,
            library_list,
            library_add,
            library_remove,
            scan_start,
            scan_stop,
            play,
            stop,
            toggle_pause,
            seek,
            set_volume,
            set_censor_style,
            set_subtitle,
            adjust_sub_delay,
            cast_discover,
            cast_start,
            cast_stop,
        ])
        .setup(|app| {
            let data_dir = app
                .path()
                .app_data_dir()
                .unwrap_or_else(|_| std::env::temp_dir().join("scrim"));
            let paths = Paths::discover(data_dir);
            let store = Store::load(paths.library_file(), paths.settings_file());

            app.manage(App {
                paths,
                store: Mutex::new(store),
                playback: Mutex::new(Playback::default()),
                mpv: tokio::sync::Mutex::new(None),
                scans: Mutex::new(HashMap::new()),
                cast: Mutex::new(None),
                cast_devices: Mutex::new(Vec::new()),
                #[cfg(windows)]
                host: Mutex::new(None),
                last_bounds: Mutex::new(None),
            });

            #[cfg(windows)]
            {
                use windows::Win32::Foundation::HWND;
                let window = app.get_webview_window("main").expect("main window");
                let hwnd = HWND(window.hwnd()?.0 as *mut _);
                let host =
                    VideoHost::new(hwnd).map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
                host.hide(); // nothing is playing yet
                *app.state::<App>().host.lock().unwrap() = Some(host);
            }

            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let mut timer = tokio::time::interval(Duration::from_millis(250));
                loop {
                    timer.tick().await;
                    tick(handle.clone()).await;
                }
            });

            Ok(())
        })
        .on_window_event(|window, event| match event {
            tauri::WindowEvent::Moved(_) | tauri::WindowEvent::Resized(_) => {
                #[cfg(windows)]
                {
                    let app = window.state::<App>();
                    let rect = *app.last_bounds.lock().unwrap();
                    let guard = app.host.lock().unwrap();
                    if let Some(host) = guard.as_ref() {
                        match rect {
                            Some((x, y, w, h)) => host.set_bounds(x, y, w, h),
                            None => host.fill_parent(),
                        }
                    }
                }
            }
            tauri::WindowEvent::CloseRequested { .. } | tauri::WindowEvent::Destroyed => {
                let app = window.state::<App>();
                app.store.lock().unwrap().save();
                if let Some(mut c) = app.cast.lock().unwrap().take() {
                    c.stop_cast();
                }
                for (_, scan) in app.scans.lock().unwrap().drain() {
                    scan.stop();
                }
                #[cfg(windows)]
                drop(app.host.lock().unwrap().take());
            }
            _ => {}
        })
        .run(tauri::generate_context!())
        .expect("error while running Scrim");
}
