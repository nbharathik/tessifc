// SPDX-License-Identifier: Apache-2.0
// Code is highlighted during the build. Add a readable language label without
// loading a client-side syntax highlighter or changing the copied source.
(() => {
  document.addEventListener("change", (event) => {
    const input = event.target;
    if (!(input instanceof HTMLInputElement) || input.name !== "__palette") return;
    const mode = ["system", "light", "dark"][Number(input.id.split("_").pop())];
    if (mode) {
      try { localStorage.setItem("tessifc.theme", mode); } catch { /* Optional persistence. */ }
    }
  });
  const languages = {
    js: "JavaScript", javascript: "JavaScript", ts: "TypeScript",
    typescript: "TypeScript", sh: "Shell", bash: "Shell", shell: "Shell",
    rust: "Rust", toml: "TOML", json: "JSON", python: "Python",
    yaml: "YAML", text: "Text", console: "Terminal", html: "HTML", css: "CSS",
  };
  function labelCode() {
    document.querySelectorAll(".highlight").forEach((block) => {
      if (block.querySelector(":scope > .filename") || block.dataset.labelled) return;
      const language = [...block.classList].find((name) => name.startsWith("language-"))?.slice(9);
      if (!language) return;
      const label = document.createElement("span");
      label.className = "filename";
      label.textContent = languages[language] || language;
      block.prepend(label);
      block.dataset.labelled = "true";
    });
  }
  labelCode();
  // Also supports Material's instant navigation if enabled in the future.
  if (typeof document$ !== "undefined") document$.subscribe(labelCode);
})();
