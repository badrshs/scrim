// Scrim front end. Compositing spike stage.
//
// The one job that matters here: keep mpv's native child window exactly under
// the hole in the UI, at every size and DPI. Everything else is scaffolding
// until the spike is confirmed.

const invoke = window.__TAURI__?.core?.invoke;
const el = (id) => document.getElementById(id);

/* ------------------------------------------------------------- stage sync -- */

let lastBounds = "";

function syncStage() {
  const video = el("stage-video");
  if (!video || !invoke) return;

  const r = video.getBoundingClientRect();
  const bounds = {
    x: r.left,
    y: r.top,
    width: r.width,
    height: r.height,
    scale: window.devicePixelRatio || 1,
  };

  // Win32 window moves are not free, and this runs on every resize frame.
  const key = `${bounds.x}|${bounds.y}|${bounds.width}|${bounds.height}|${bounds.scale}`;
  if (key === lastBounds) return;
  lastBounds = key;

  invoke("set_stage_bounds", { bounds }).catch(console.error);
}

// getBoundingClientRect after layout, every frame the size actually changes.
const ro = new ResizeObserver(() => requestAnimationFrame(syncStage));
ro.observe(document.documentElement);
window.addEventListener("resize", () => requestAnimationFrame(syncStage));
window.addEventListener("DOMContentLoaded", syncStage);
requestAnimationFrame(syncStage);

/* ------------------------------------------------------------ window chrome */

const appWindow = window.__TAURI__?.window?.getCurrentWindow?.();

document.querySelectorAll("[data-win]").forEach((btn) => {
  btn.addEventListener("click", () => {
    if (!appWindow) return;
    const action = btn.dataset.win;
    if (action === "minimize") appWindow.minimize();
    if (action === "maximize") appWindow.toggleMaximize();
    if (action === "close") appWindow.close();
  });
});

/* ------------------------------------------------------------- spike drive -- */

const CENSOR_STYLES = [
  ["black_box", "Black box"],
  ["white_box", "White box"],
  ["blur_strong", "Blur strong"],
  ["blur_medium", "Blur medium"],
  ["blur_light", "Blur light"],
];
let styleIndex = 0;

function setStatus(l1, l2, l3) {
  if (l1 !== undefined) el("status-line1").textContent = l1;
  if (l2 !== undefined) el("status-line2").textContent = l2;
  if (l3 !== undefined) el("status-line3").textContent = l3;
}

// Draw one tick per censor window on the seek bar. This is the element the
// whole design hangs on: you can see how much of the movie carries a cover
// before you press play.
function drawWindows(windows, duration) {
  const host = el("seek-windows");
  host.innerHTML = "";
  if (!duration) return;
  for (const w of windows) {
    const tick = document.createElement("div");
    tick.className = "seek__window";
    tick.style.left = `${(w[0] / duration) * 100}%`;
    tick.style.width = `${Math.max(0.15, ((w[1] - w[0]) / duration) * 100)}%`;
    host.appendChild(tick);
  }
}

async function spikePlay(videoPath, planPath) {
  if (!invoke) {
    setStatus("NO BRIDGE", "window.__TAURI__ is missing", "");
    return;
  }
  const [styleKey, styleLabel] = CENSOR_STYLES[styleIndex];
  el("censor-label").textContent = styleLabel;

  try {
    const graph = planPath ? await invoke("graph_for_plan", { planPath, style: styleKey }) : "";
    const msg = await invoke("spike_play", { path: videoPath, graph });

    el("stage-empty").classList.add("hidden");
    // Stop painting the stage so mpv's window shows through it.
    el("stage").classList.remove("is-idle");
    el("now-playing").textContent = videoPath.split(/[\\/]/).pop();
    setStatus(
      graph ? "CENSOR ACTIVE" : "NO COVER NEEDED",
      `${styleLabel} · graph ${graph.length} chars`,
      msg
    );
  } catch (e) {
    setStatus("SPIKE FAILED", String(e), "");
    console.error(e);
  }
}

el("btn-censor").addEventListener("click", () => {
  styleIndex = (styleIndex + 1) % CENSOR_STYLES.length;
  const [, label] = CENSOR_STYLES[styleIndex];
  el("censor-label").textContent = label;
  if (window.__scrimSpike) spikePlay(...window.__scrimSpike);
});

// The spike auto-plays whatever it was handed, so the window can be
// screenshotted without any interaction.
window.__scrimSpikeStart = (videoPath, planPath) => {
  window.__scrimSpike = [videoPath, planPath];
  spikePlay(videoPath, planPath);
};

// Report readiness so the harness knows the bridge is alive.
setStatus("STAGE READY", `dpr ${window.devicePixelRatio}`, "waiting for a movie");

// SPIKE ONLY: auto-play the real test movie at a timestamp that is actually
// covered, so a screenshot shows a censor box sitting under the HTML controls.
// Removed once compositing is confirmed.
const SPIKE_VIDEO = "C:\\Users\\bader\\Repos\\movie-plur\\abc.mp4";
const SPIKE_PLAN =
  "C:\\Users\\bader\\Repos\\movie-plur\\crates\\scrim-core\\tests\\fixtures\\abc.plan.json";
setTimeout(() => window.__scrimSpikeStart(SPIKE_VIDEO, SPIKE_PLAN), 400);
