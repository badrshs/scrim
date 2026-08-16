// Light, dark, or whatever the system is doing.
//
// The theme is a class on <html> rather than a media query, because the same
// palette definition has to be attachable to two different things: the whole
// document, and the video stage, which stays dark in every theme. See the note
// in tokens.css.

const media = window.matchMedia("(prefers-color-scheme: dark)");
let preference = "system";

export function initTheme(saved) {
  preference = saved || "system";
  apply();
  // Following the system means following it as it changes, not only at launch.
  media.addEventListener("change", () => {
    if (preference === "system") apply();
  });
}

export function setTheme(next) {
  preference = next;
  apply();
}

function apply() {
  const dark = preference === "dark" || (preference === "system" && media.matches);
  const root = document.documentElement;
  root.classList.toggle("theme-dark", dark);
  root.classList.toggle("theme-light", !dark);
}
