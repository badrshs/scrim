// Scrim's interface.
//
// Plain modules, no framework and no build step: the whole frontend is static
// files that Tauri serves directly, so building Scrim needs a Rust toolchain
// and nothing else.
//
// Rust owns the truth. This file sends intentions and renders whatever the
// `playback` event reports, rather than keeping its own idea of what is
// happening. That matters most for the safety fences: the interface must never
// be able to show "playing" while Rust has playback held.

import { fmtClock, fmtShort } from "./format.js";
import { initTheme, setTheme } from "./theme.js";

const tauri = window.__TAURI__;
const invoke = tauri?.core?.invoke;
const listen = tauri?.event?.listen;
const dialog = tauri?.dialog;
const appWindow = tauri?.window?.getCurrentWindow?.();

const $ = (id) => document.getElementById(id);
const on = (id, ev, fn) => $(id)?.addEventListener(ev, fn);

const VIDEO_EXTENSIONS = ["mp4", "mkv", "avi", "mov", "webm", "m4v", "wmv", "flv", "mpg", "mpeg"];
const SUB_EXTENSIONS = ["srt", "ass", "ssa", "vtt", "sub"];

const CENSOR_STYLES = [
  { key: "black_box", label: "Black box", hint: "default", swatch: "swatch--black" },
  { key: "white_box", label: "White box", hint: "", swatch: "swatch--white" },
  { key: "blur_strong", label: "Blur strong", hint: "see-through", swatch: "swatch--blur swatch--blur-strong" },
  { key: "blur_medium", label: "Blur medium", hint: "see-through", swatch: "swatch--blur swatch--blur-medium" },
  { key: "blur_light", label: "Blur light", hint: "see-through", swatch: "swatch--blur swatch--blur-light" },
];

const state = {
  playback: {},
  library: [],
  selected: null,
  filter: "",
  scrubbing: false,
  fullscreen: false,
  settings: {},
};

/* =============================================================== stage === */

let lastBounds = "";

function syncStage() {
  const video = $("stage-video");
  if (!video || !invoke) return;
  const r = video.getBoundingClientRect();
  const bounds = {
    x: r.left,
    y: r.top,
    width: r.width,
    height: r.height,
    scale: window.devicePixelRatio || 1,
  };
  const key = `${bounds.x}|${bounds.y}|${bounds.width}|${bounds.height}|${bounds.scale}`;
  if (key === lastBounds) return;
  lastBounds = key;
  invoke("set_stage_bounds", { bounds }).catch(() => {});
}

new ResizeObserver(() => requestAnimationFrame(syncStage)).observe(document.documentElement);
window.addEventListener("resize", () => requestAnimationFrame(syncStage));

/* ============================================================== errors === */

let errorTimer = null;

function toast(message, tone = "warn") {
  const bar = $("status-engine");
  if (!bar) return;
  bar.textContent = message;
  bar.style.color = tone === "warn" ? "var(--warn)" : "var(--accent)";
  clearTimeout(errorTimer);
  errorTimer = setTimeout(() => {
    bar.style.color = "";
    bar.textContent = state.playback.playing ? "playing" : "ready";
  }, 6000);
}

async function call(cmd, args) {
  try {
    return await invoke(cmd, args);
  } catch (e) {
    toast(String(e));
    throw e;
  }
}

/* ============================================================== render === */

function renderPlayback(p) {
  state.playback = p;

  const playing = !!p.playing;
  $("btn-play").disabled = !playing;
  $("btn-stop").disabled = !playing;
  $("btn-subs").disabled = !playing;
  $("btn-cast").disabled = !p.file;
  $("btn-play").innerHTML = p.paused ? "&#9654;" : "&#10074;&#10074;";

  $("now-playing").textContent = p.name || "nothing playing";
  $("stage").classList.toggle("is-idle", !playing);
  $("stage-empty").classList.toggle("hidden", !!playing);
  $("controls").classList.toggle("hidden", false);

  // plan badge
  const badge = $("plan-badge");
  if (p.file && p.windows) {
    badge.classList.remove("hidden");
    if (p.live && !p.scanComplete) {
      badge.className = "badge badge--warn";
      badge.textContent = `LIVE · ${p.windows.length} COVERED`;
    } else {
      badge.className = "badge badge--ok";
      badge.textContent = `PLAN OK · ${p.windows.length} COVERED`;
    }
  } else {
    badge.classList.add("hidden");
  }

  // time and seek
  $("time-now").textContent = fmtClock(p.position);
  $("time-total").textContent = fmtClock(p.duration);
  if (!state.scrubbing && p.duration > 0) {
    const pct = Math.min(100, (p.position / p.duration) * 100);
    $("seek-played").style.width = `${pct}%`;
    $("seek-handle").style.left = `${pct}%`;
    $("seek").setAttribute("aria-valuenow", Math.round(pct));
  }

  renderSeekWindows(p);

  // volume
  const vol = p.volume ?? 100;
  $("volume-fill").style.width = `${(vol / 130) * 100}%`;
  $("volume-handle").style.left = `${(vol / 130) * 100}%`;
  $("volume-value").textContent = vol;

  // subtitles
  $("sub-name").textContent = p.subtitle ? baseName(p.subtitle) : "none";
  $("sub-file-name").textContent = p.subtitle ? baseName(p.subtitle) : "No subtitle attached";
  $("sub-attached").classList.toggle("hidden", !p.subtitle);
  $("sub-delay-value").textContent = `${(p.subDelay ?? 0).toFixed(2)} s`;
  const chip = $("sub-delay-chip");
  chip.classList.toggle("hidden", !p.subDelay);
  chip.textContent = `${p.subDelay > 0 ? "+" : ""}${(p.subDelay ?? 0).toFixed(2)}s`;

  // censor label
  const style = CENSOR_STYLES.find((s) => s.key === p.censorStyle) || CENSOR_STYLES[0];
  $("censor-label").textContent = style.label;

  renderLive(p);
  renderFence(p);
  renderStatus(p);
  renderCoverReason(p);

  $("mode-label").textContent = !p.file
    ? ""
    : p.live
      ? p.scanComplete
        ? "live detection · complete"
        : "live detection mode"
      : "scanned plan mode";

  if (p.casting) {
    $("btn-cast").textContent = "STOP CAST";
    $("btn-cast").classList.add("btn--secondary");
  } else {
    $("btn-cast").textContent = "CAST";
    $("btn-cast").classList.remove("btn--secondary");
  }
}

// The signature element: every covered span drawn on the scrub bar, plus the
// hatched region live detection has not reached.
function renderSeekWindows(p) {
  const host = $("seek-windows");
  const key = `${p.file}|${p.windows?.length}|${p.duration}`;
  if (host.dataset.key !== key) {
    host.dataset.key = key;
    host.innerHTML = "";
    if (p.duration > 0) {
      for (const [start, end] of p.windows || []) {
        const tick = document.createElement("div");
        tick.className = "seek__window";
        tick.style.left = `${(start / p.duration) * 100}%`;
        tick.style.width = `${Math.max(0.12, ((end - start) / p.duration) * 100)}%`;
        host.appendChild(tick);
      }
    }
  }

  const showFrontier = p.live && !p.scanComplete && p.duration > 0;
  const pct = showFrontier ? Math.min(100, (p.frontier / p.duration) * 100) : 100;
  $("seek-unscanned").classList.toggle("hidden", !showFrontier);
  $("seek-frontier").classList.toggle("hidden", !showFrontier);
  $("seek-unscanned").style.left = `${pct}%`;
  $("seek-frontier").style.left = `${pct}%`;
}

function renderLive(p) {
  const show = p.live && !p.scanComplete && p.file;
  $("livebar").classList.toggle("hidden", !show);
  $("status").classList.toggle("status--offset", !!show);
  if (!show) return;

  const pct = p.duration > 0 ? (p.frontier / p.duration) * 100 : 0;
  $("live-fill").style.width = `${pct}%`;
  $("live-stats").textContent =
    `3 fps · ${p.detections} found · frontier ${fmtClock(p.frontier)} · ${p.scanSpeed.toFixed(1)}x realtime`;
  $("live-eta").textContent = p.playing
    ? `${pct.toFixed(0)}% analysed`
    : `playback starts in ${fmtShort(p.headStartRemaining)}`;
}

function renderFence(p) {
  const show = !!p.fenced;
  $("fence").classList.toggle("hidden", !show);
  if (!show) return;
  $("fence-body").textContent =
    "Playback reached the point detection has looked up to. It resumes on its own once the scanner is far enough ahead.";
  $("fence-pos").textContent = `position ${fmtClock(p.position)}`;
  $("fence-frontier").textContent = `analysed to ${fmtClock(p.frontier)}`;
  $("fence-resume").textContent =
    p.fenceResumeIn != null ? `resuming in ${fmtShort(p.fenceResumeIn)}` : "waiting for the scanner";
}

function renderStatus(p) {
  const show = !!p.file && !$("settings").classList.contains("hidden") === false;
  $("status").classList.toggle("hidden", !p.file);
  if (!p.file) return;
  const style = CENSOR_STYLES.find((s) => s.key === p.censorStyle) || CENSOR_STYLES[0];
  $("status-1").textContent = p.windows?.length ? "COVER ACTIVE" : "NOTHING FLAGGED";
  $("status-2").textContent =
    `${style.label} · lead ${state.settings.leadBefore ?? 5}s · hold ${state.settings.holdAfter ?? 10}s`;
  $("status-3").textContent =
    `${p.windows?.length ?? 0} covered spans · ${fmtShort(p.coveredSeconds)} of runtime`;
  $("status-engine").textContent = p.playing ? (p.paused ? "paused" : "playing") : "ready";
  void show;
}

// Say why the picture is covered, while it is covered.
//
// The detector only ever reports nudity, so a cover during a fight scene is a
// false positive rather than violence detection. Naming the label and its
// confidence is what lets someone tell those apart, and a run seen in a single
// sampled frame is called out because that is the shape a false positive takes.
function renderCoverReason(p) {
  const el = $("cover-why");
  const r = p.cover;
  if (!r || !p.playing) {
    el.classList.add("hidden");
    return;
  }

  el.classList.remove("hidden");
  el.classList.toggle("is-thin", r.detections === 1);

  // Plans written before reasons were recorded have nothing to say.
  $("cover-label").textContent = r.label || "COVERED";
  $("cover-score").textContent = r.peakScore ? r.peakScore.toFixed(2) : "";

  const seen =
    r.detections === 1
      ? "seen once"
      : `seen ${r.detections}x over ${((r.detections - 1) / 3).toFixed(1)}s`;
  $("cover-meta").textContent = `${seen} · from ${fmtClock(r.firstSeen)}`;
  el.title =
    r.detections === 1
      ? "Detected in a single sampled frame. Isolated hits like this are often " +
        "bare skin mistaken for nudity. Settings: require 2 detections in a row to ignore them."
      : "Detected across several sampled frames in a row.";
}

function baseName(p) {
  return String(p).split(/[\\/]/).pop();
}

/* ============================================================= library === */

async function refreshLibrary() {
  state.library = await invoke("library_list").catch(() => []);
  renderLibrary();
}

function renderLibrary() {
  const list = $("library-list");
  const filter = state.filter.toLowerCase();
  const movies = state.library.filter((m) => m.name.toLowerCase().includes(filter));

  const ready = state.library.filter((m) => m.status === "ready").length;
  const scanning = state.library.filter((m) => m.status === "scanning").length;
  $("library-meta").textContent =
    `${state.library.length} ${state.library.length === 1 ? "file" : "files"} · ${ready} ready` +
    (scanning ? ` · ${scanning} scanning` : "");

  list.innerHTML = "";
  if (!movies.length) {
    const empty = document.createElement("div");
    empty.className = "mono";
    empty.style.cssText = "color:var(--text-6);padding:18px 4px;line-height:1.6";
    empty.textContent = state.library.length
      ? "Nothing matches that filter."
      : "No movies yet. Add some to get started.";
    list.appendChild(empty);
    return;
  }

  for (const m of movies) {
    list.appendChild(movieCard(m));
  }
}

function movieCard(m) {
  const card = document.createElement("div");
  card.className = "movie";
  if (m.status === "scanning") card.classList.add("is-scanning");
  if (state.selected === m.path) card.classList.add("is-selected");

  const row = document.createElement("div");
  row.className = "movie__row";
  const name = document.createElement("span");
  name.className = "movie__name";
  name.textContent = m.name;
  name.title = m.path;
  row.appendChild(name);
  row.appendChild(statusBadge(m));
  card.appendChild(row);

  const meta = document.createElement("div");
  meta.className = "movie__meta";
  meta.textContent = metaLine(m);
  card.appendChild(meta);

  if (m.status === "scanning") {
    const bar = document.createElement("div");
    bar.className = "progress";
    const fill = document.createElement("div");
    fill.className = "progress__fill";
    fill.style.width = `${m.scanPercent}%`;
    bar.appendChild(fill);
    card.appendChild(bar);
  }

  const actions = document.createElement("div");
  actions.className = "movie__actions";

  if (m.status === "ready") {
    actions.appendChild(button("Play", "btn btn--sm btn--primary", () => play(m.path, "scanned")));
    actions.appendChild(button("Rescan", "btn btn--sm", () => startScan(m.path)));
  } else if (m.status === "scanning") {
    actions.appendChild(
      button("Watch as it scans", "btn btn--sm btn--secondary", () => play(m.path, "live"))
    );
    actions.appendChild(button("Stop scan", "btn btn--sm", () => call("scan_stop", { path: m.path }).then(refreshLibrary)));
  } else {
    actions.appendChild(button("Scan", "btn btn--sm btn--primary", () => startScan(m.path)));
    actions.appendChild(
      button("Watch with live detection", "btn btn--sm btn--secondary", () => play(m.path, "live"))
    );
  }
  actions.appendChild(
    button("Remove", "btn btn--sm", async () => {
      await call("library_remove", { path: m.path });
      refreshLibrary();
    })
  );
  card.appendChild(actions);

  card.addEventListener("click", () => {
    state.selected = m.path;
    renderLibrary();
  });
  return card;
}

function statusBadge(m) {
  const b = document.createElement("span");
  if (m.status === "ready") {
    b.className = "badge badge--ok";
    b.textContent = "READY";
  } else if (m.status === "scanning") {
    b.className = "badge badge--warn";
    b.textContent = `SCANNING ${m.scanPercent.toFixed(0)}%`;
  } else if (m.status === "unreadable") {
    b.className = "badge badge--warn";
    b.textContent = "PLAN UNREADABLE";
  } else {
    b.className = "badge badge--idle";
    b.textContent = "NOT SCANNED";
  }
  return b;
}

function metaLine(m) {
  if (m.status === "scanning") {
    const left = m.duration > 0 ? (m.duration * (1 - m.scanPercent / 100)) / Math.max(m.scanSpeed, 0.1) : 0;
    return `${m.scanSpeed.toFixed(1)}x realtime · about ${fmtShort(left)} left`;
  }
  if (m.status === "ready") {
    return `${fmtClock(m.duration)} · ${m.windows} covered spans · ${fmtShort(m.coveredSeconds)} of runtime`;
  }
  if (m.status === "unreadable") {
    return m.error || "the plan file could not be read, so playing it is refused";
  }
  return "no scan yet · scanned-plan playback is refused until there is one";
}

function button(label, cls, onClick) {
  const b = document.createElement("button");
  b.className = cls;
  b.textContent = label;
  b.addEventListener("click", (e) => {
    e.stopPropagation();
    onClick();
  });
  return b;
}

/* ============================================================= actions === */

async function play(path, mode) {
  closeDrawer();
  try {
    await call("play", { path, mode });
  } catch {
    /* already reported */
  }
}

async function startScan(path) {
  await call("scan_start", { path });
  toast("Scanning started. You can keep using Scrim.", "ok");
  refreshLibrary();
}

async function addMovies() {
  if (!dialog) return;
  const picked = await dialog.open({
    multiple: true,
    filters: [{ name: "Videos", extensions: VIDEO_EXTENSIONS }],
  });
  if (!picked) return;
  const paths = Array.isArray(picked) ? picked : [picked];
  await call("library_add", { paths });
  refreshLibrary();
}

function openDrawer() {
  $("drawer").classList.remove("hidden");
  refreshLibrary();
  $("library-filter").focus();
}

function closeDrawer() {
  $("drawer").classList.add("hidden");
}

function closePopovers() {
  $("pop-censor").classList.add("hidden");
  $("pop-subs").classList.add("hidden");
}

async function toggleFullscreen() {
  state.fullscreen = !state.fullscreen;
  await appWindow?.setFullscreen(state.fullscreen);
  $("fs-hint").classList.toggle("hidden", !state.fullscreen);
  requestAnimationFrame(syncStage);
}

/* ============================================================== wiring === */

function renderCensorOptions() {
  const host = $("censor-options");
  host.innerHTML = "";
  for (const s of CENSOR_STYLES) {
    const opt = document.createElement("button");
    opt.className = "option";
    if (state.playback.censorStyle === s.key) opt.classList.add("is-selected");
    opt.innerHTML =
      `<span class="swatch ${s.swatch}"></span>` +
      `<span class="option__label">${s.label}</span>` +
      (s.hint ? `<span class="option__hint">${s.hint}</span>` : "") +
      (state.playback.censorStyle === s.key ? `<span class="option__check">&#10003;</span>` : "");
    opt.addEventListener("click", async () => {
      await call("set_censor_style", { style: s.key });
      renderCensorOptions();
    });
    host.appendChild(opt);
  }
}

function wireSeek() {
  const seek = $("seek");
  const posFromEvent = (e) => {
    const r = seek.getBoundingClientRect();
    const ratio = Math.max(0, Math.min(1, (e.clientX - r.left) / r.width));
    return ratio * (state.playback.duration || 0);
  };

  let lastScrub = 0;
  seek.addEventListener("pointerdown", (e) => {
    if (!state.playback.playing) return;
    state.scrubbing = true;
    // Capture lets the handle keep following the pointer past the bar's
    // edges, but it is an enhancement: if it is refused, dragging still works
    // and a plain press must never be swallowed by the exception.
    try {
      seek.setPointerCapture(e.pointerId);
    } catch {
      /* not fatal */
    }
    const t = posFromEvent(e);
    updateSeekVisual(t);
    call("seek", { seconds: t, exact: false });
  });
  seek.addEventListener("pointermove", (e) => {
    if (!state.scrubbing) return;
    const t = posFromEvent(e);
    updateSeekVisual(t);
    // Keyframe seeks while the handle moves, capped so mpv is not flooded.
    const now = performance.now();
    if (now - lastScrub > 120) {
      lastScrub = now;
      call("seek", { seconds: t, exact: false });
    }
  });
  const finish = (e) => {
    if (!state.scrubbing) return;
    state.scrubbing = false;
    try {
      seek.releasePointerCapture(e.pointerId);
    } catch {
      /* not fatal */
    }
    // The exact seek lands only once the handle is dropped: exact seeks are
    // slow, and doing one per pointermove makes scrubbing crawl.
    call("seek", { seconds: posFromEvent(e), exact: true });
  };
  seek.addEventListener("pointerup", finish);
  seek.addEventListener("pointercancel", finish);
  // A press that never produced a pointerup (capture lost, window focus
  // change) would otherwise leave the bar stuck in scrubbing forever, and the
  // position readout frozen with it.
  window.addEventListener("pointerup", (e) => {
    if (state.scrubbing) finish(e);
  });
}

function updateSeekVisual(seconds) {
  const d = state.playback.duration || 1;
  const pct = Math.max(0, Math.min(100, (seconds / d) * 100));
  $("seek-played").style.width = `${pct}%`;
  $("seek-handle").style.left = `${pct}%`;
  $("time-now").textContent = fmtClock(seconds);
}

function wireVolume() {
  const track = $("volume-track");
  let dragging = false;
  const set = (e) => {
    const r = track.getBoundingClientRect();
    const ratio = Math.max(0, Math.min(1, (e.clientX - r.left) / r.width));
    call("set_volume", { volume: Math.round(ratio * 130) });
  };
  track.addEventListener("pointerdown", (e) => {
    dragging = true;
    track.setPointerCapture(e.pointerId);
    set(e);
  });
  track.addEventListener("pointermove", (e) => dragging && set(e));
  track.addEventListener("pointerup", (e) => {
    dragging = false;
    track.releasePointerCapture(e.pointerId);
  });
}

async function openCast() {
  if (state.playback.casting) {
    await call("cast_stop");
    return;
  }
  $("cast-modal").classList.remove("hidden");
  $("cast-meta").textContent = "searching…";
  $("cast-devices").innerHTML = "";
  $("cast-start").disabled = true;

  let devices = [];
  try {
    devices = await invoke("cast_discover");
  } catch (e) {
    $("cast-meta").textContent = String(e);
    return;
  }

  $("cast-meta").textContent = `${devices.length} found`;
  if (!devices.length) {
    $("cast-devices").innerHTML =
      `<div class="mono" style="color:var(--text-6);padding:10px">No devices answered. They must be on the same network as this computer.</div>`;
    return;
  }

  let chosen = devices[0].host;
  const draw = () => {
    $("cast-devices").innerHTML = "";
    for (const d of devices) {
      const row = document.createElement("button");
      row.className = "device" + (d.host === chosen ? " is-selected" : "");
      row.innerHTML =
        `<span class="radio">${d.host === chosen ? '<span class="radio__dot"></span>' : ""}</span>` +
        `<span class="device__name"></span><span class="device__ip"></span>`;
      row.querySelector(".device__name").textContent = d.name;
      row.querySelector(".device__ip").textContent = d.host;
      row.addEventListener("click", () => {
        chosen = d.host;
        draw();
      });
      $("cast-devices").appendChild(row);
    }
  };
  draw();
  $("cast-start").disabled = false;
  $("cast-start").onclick = async () => {
    $("cast-start").disabled = true;
    $("cast-meta").textContent = "connecting…";
    try {
      const name = await invoke("cast_start", { host: chosen });
      $("cast-modal").classList.add("hidden");
      toast(`Casting to ${name}, with the cover burned in.`, "ok");
    } catch (e) {
      $("cast-meta").textContent = String(e);
      $("cast-start").disabled = false;
    }
  };
}

// Ranges mirror Settings::sanitised in Rust, which is the authority.
const TUNE_LIMITS = {
  leadBefore: [0, 30],
  holdAfter: [0, 60],
  headStartMinutes: [1, 10],
  threshold: [0.3, 0.9],
  minRun: [1, 10],
};

function renderSettings() {
  const s = state.settings;
  $("v-lead").textContent = `${(s.leadBefore ?? 5).toFixed(1)} s`;
  $("v-hold").textContent = `${(s.holdAfter ?? 10).toFixed(1)} s`;
  $("v-head").textContent = `${s.headStartMinutes ?? 5} min`;
  $("v-threshold").textContent = (s.threshold ?? 0.55).toFixed(2);
  $("v-margin").textContent = `${Math.round((s.margin ?? 0.08) * 100)}%`;

  const minRun = s.minRun ?? 1;
  $("v-minrun").textContent = minRun;
  $("minrun-hint").textContent =
    minRun <= 1
      ? "covering every glimpse, including single frames that are often false positives"
      : `runs seen fewer than ${minRun} times in a row are ignored · ${((minRun - 1) / 3).toFixed(1)}s of evidence needed`;

  for (const b of document.querySelectorAll("#theme-switch .segmented__item")) {
    b.classList.toggle("is-active", b.dataset.theme === (s.theme || "system"));
  }
}

async function tune(key, step) {
  const [lo, hi] = TUNE_LIMITS[key] || [0, 1e9];
  const current = state.settings[key] ?? 0;
  // Float steps accumulate visible drift (0.55 - 0.05 = 0.4999...).
  const next = Math.min(hi, Math.max(lo, Math.round((current + step) * 1000) / 1000));
  if (next === current) return;
  state.settings[key] = next;
  renderSettings();
  // Rust re-derives coverage from the plan's detections and pushes the new
  // filtergraph, so this applies to a movie already playing.
  await call("set_settings", { settings: state.settings });
}

/* ================================================================ boot === */

async function boot() {
  if (!invoke) {
    document.body.innerHTML =
      '<div style="padding:40px;font-family:monospace">Scrim must be run as an application, not opened in a browser.</div>';
    return;
  }

  const info = await invoke("app_info");
  state.settings = info.settings;
  $("version-label").textContent = `Scrim ${info.version}`;
  initTheme(info.settings.theme);
  renderSettings();
  renderCensorOptions();

  if (info.missing.length) {
    const names = info.missing.map((m) => m.name).join(", ");
    toast(`Missing from this install: ${names}. Run tools/fetch-resources.ps1.`);
    $("empty-title").textContent = "Scrim is missing some of its own files";
    $("empty-hint").textContent = `${names} — see the README`;
  }

  await refreshLibrary();

  listen?.("playback", (e) => renderPlayback(e.payload));
  listen?.("library-changed", () => refreshLibrary());
  listen?.("error", (e) => toast(String(e.payload)));

  // The library list also reflects scan progress, which Rust does not push.
  setInterval(() => {
    if (!$("drawer").classList.contains("hidden")) refreshLibrary();
  }, 1000);

  requestAnimationFrame(syncStage);
  if (!state.library.length) openDrawer();
}

// window chrome
document.querySelectorAll("[data-win]").forEach((btn) => {
  btn.addEventListener("click", () => {
    const a = btn.dataset.win;
    if (a === "minimize") appWindow?.minimize();
    if (a === "maximize") appWindow?.toggleMaximize();
    if (a === "close") appWindow?.close();
  });
});

on("btn-play", "click", () => call("toggle_pause"));
on("btn-stop", "click", () => call("stop"));
on("btn-library", "click", () => ($("drawer").classList.contains("hidden") ? openDrawer() : closeDrawer()));
on("btn-close-drawer", "click", closeDrawer);
on("btn-add", "click", addMovies);
on("btn-scan-all", "click", async () => {
  for (const m of state.library.filter((m) => m.status === "unscanned")) {
    await call("scan_start", { path: m.path }).catch(() => {});
  }
  refreshLibrary();
});
on("library-filter", "input", (e) => {
  state.filter = e.target.value;
  renderLibrary();
});

on("btn-censor", "click", (e) => {
  e.stopPropagation();
  const pop = $("pop-censor");
  const opening = pop.classList.contains("hidden");
  closePopovers();
  if (opening) {
    renderCensorOptions();
    pop.classList.remove("hidden");
  }
});
on("btn-subs", "click", (e) => {
  e.stopPropagation();
  const pop = $("pop-subs");
  const opening = pop.classList.contains("hidden");
  closePopovers();
  if (opening) pop.classList.remove("hidden");
});
document.addEventListener("click", (e) => {
  if (!e.target.closest(".pop")) closePopovers();
});

on("sub-minus", "click", () => call("adjust_sub_delay", { delta: -0.25 }));
on("sub-plus", "click", () => call("adjust_sub_delay", { delta: 0.25 }));
on("sub-remove", "click", () => call("set_subtitle", { path: null }));
on("sub-attach", "click", async () => {
  const picked = await dialog?.open({ filters: [{ name: "Subtitles", extensions: SUB_EXTENSIONS }] });
  if (picked) await call("set_subtitle", { path: picked });
});

on("btn-cast", "click", openCast);
on("cast-cancel", "click", () => $("cast-modal").classList.add("hidden"));
on("btn-fullscreen", "click", toggleFullscreen);
on("btn-settings", "click", () => {
  $("settings").classList.remove("hidden");
  renderSettings();
});
on("settings-close", "click", () => $("settings").classList.add("hidden"));

document.querySelectorAll("[data-tune]").forEach((b) => {
  b.addEventListener("click", () => tune(b.dataset.tune, parseFloat(b.dataset.step)));
});

document.querySelectorAll("#theme-switch .segmented__item").forEach((b) => {
  b.addEventListener("click", async () => {
    state.settings.theme = b.dataset.theme;
    setTheme(b.dataset.theme);
    renderSettings();
    await call("set_settings", { settings: state.settings });
  });
});

// keyboard
document.addEventListener("keydown", (e) => {
  if (e.target.tagName === "INPUT") return;
  if (e.key === " ") {
    e.preventDefault();
    call("toggle_pause");
  } else if (e.key === "Escape") {
    if (state.fullscreen) toggleFullscreen();
    else if (!$("settings").classList.contains("hidden")) $("settings").classList.add("hidden");
    else if (!$("cast-modal").classList.contains("hidden")) $("cast-modal").classList.add("hidden");
    else closeDrawer();
  } else if (e.key === "f" || e.key === "F") {
    toggleFullscreen();
  } else if (e.key === "ArrowRight") {
    call("seek", { seconds: (state.playback.position || 0) + 10, exact: true });
  } else if (e.key === "ArrowLeft") {
    call("seek", { seconds: Math.max(0, (state.playback.position || 0) - 10), exact: true });
  }
});

wireSeek();
wireVolume();
boot();
