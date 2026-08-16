// Formatting for the numbers on screen.
//
// Every duration in this interface is something the viewer may need to trust
// (how far detection has looked, how long until playback resumes), so they are
// rendered consistently and never rounded in a way that flatters.

/** `01:23:45`, or `--:--:--` when there is nothing to show. */
export function fmtClock(seconds) {
  if (seconds == null || !isFinite(seconds) || seconds < 0) return "--:--:--";
  const s = Math.floor(seconds);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  return `${pad(h)}:${pad(m)}:${pad(sec)}`;
}

/** Compact and human: `45s`, `2:05`, `1h 12m`. For estimates and countdowns. */
export function fmtShort(seconds) {
  if (seconds == null || !isFinite(seconds) || seconds < 0) return "--";
  const s = Math.round(seconds);
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}:${pad(s % 60)}`;
  const h = Math.floor(s / 3600);
  const m = Math.round((s % 3600) / 60);
  return `${h}h ${m}m`;
}

function pad(n) {
  return String(n).padStart(2, "0");
}
