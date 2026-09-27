// Paints the stored theme before the app's own script runs, so a light-theme
// window never flashes dark on launch. A classic script in <head>, not inline
// (the CSP allows only 'self' scripts), and deliberately tiny: it must agree
// with `resolveTheme` in src/theme/theme.ts, which `theme_boot.test.ts` checks
// for every stored value and system preference.
(function () {
  var preference = "system";
  try {
    var stored = window.localStorage.getItem("ainb.theme");
    if (stored === "dark" || stored === "light" || stored === "system") preference = stored;
  } catch (error) {
    // Storage blocked or missing: the system's theme, as the app does.
  }
  var systemPrefersDark = true;
  try {
    if (window.matchMedia) systemPrefersDark = window.matchMedia("(prefers-color-scheme: dark)").matches;
  } catch (error) {
    // No media queries: dark, as the app defaults.
  }
  var theme = preference === "system" ? (systemPrefersDark ? "dark" : "light") : preference;
  var root = document.documentElement;
  root.classList.toggle("dark", theme === "dark");
  root.classList.toggle("light", theme === "light");
})();
