// SPDX-License-Identifier: Apache-2.0

// Resolves the saved theme before first paint. A classic script, not a module,
// so it runs before the splash is painted; a separate file so the page can
// ship a Content-Security-Policy with no inline script allowance.
(() => {
  let mode = "system";
  try {
    // Storage can be blocked in privacy modes; a blocked read keeps the default.
    const saved = localStorage.getItem("tessifc.theme");
    if (saved === "light" || saved === "dark" || saved === "system") mode = saved;
  } catch {
  }
  const dark = mode === "dark" || (mode === "system" && !matchMedia("(prefers-color-scheme: light)").matches);
  document.documentElement.dataset.theme = dark ? "dark" : "light";
  document.documentElement.dataset.themeMode = mode;
})();
