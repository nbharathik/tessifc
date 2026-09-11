// SPDX-License-Identifier: Apache-2.0

//! Application chrome: ribbon, panels, rail, dialogs, toasts, command
//! registry, palette and keyboard routing. Every action is a registered
//! command, so buttons, palette and keyboard all dispatch the same thing.

const $ = (id) => document.getElementById(id);

// Button id to command; buttons sharing a command share its state.
const BUTTON_COMMANDS = {
  "cmd-open": "open",
  "cmd-update": "update-ifc",
  "dz-open": "open",
  "cmd-export": "export",
  "cmd-export-2": "export",
  "save-revision": "export",
  "cmd-close": "close",
  "cmd-settings": "settings",
  "open-settings": "settings",
  "cmd-help": "help",
  "open-help": "help",
  "cmd-fit": "fit",
  "dock-fit": "fit",
  "dock-zoom-in": "zoom-in",
  "dock-zoom-out": "zoom-out",
  "cmd-plan": "plan",
  "cmd-style": "style",
  "dock-style": "style",
  "cmd-theme": "theme",
  "cmd-theme-2": "theme",
  "cmd-canvas-theme": "canvas-theme",
  "cmd-spaces": "spaces",
  "cmd-openings": "openings",
  "cmd-references": "references",
  "cmd-isolate": "isolate",
  "dock-isolate": "isolate",
  "cmd-hide": "hide",
  "dock-hide": "hide",
  "cmd-focus": "focus",
  "dock-focus": "focus",
  "cmd-edit": "edit",
  "dock-edit": "edit",
  "rail-editor": "toggle-editor",
  "rail-script": "session-script",
  "rail-assistant": "session-assistant",
  "selection-details": "element",
  "cmd-show-all": "show-all",
  "tree-restore": "show-all",
  "cmd-clear": "clear-selection",
  "selection-clear": "clear-selection",
  "cmd-measure": "measure",
  "cmd-measure-2": "measure",
  "dock-measure": "measure",
  "cmd-section": "section",
  "cmd-section-2": "section",
  "dock-section": "section",
  "outliner-close": "toggle-outliner",
  "outliner-open": "toggle-outliner",
  "inspector-close": "toggle-inspector",
  "cmd-quality": "quality",
  "cmd-properties": "properties",
  "cmd-model-stats": "model-stats",
  "tree-expand": "tree-expand",
  "tree-collapse": "tree-collapse",
  "selection-edit": "edit",
};

const PANEL_TITLES = {
  properties: "Properties",
  element: "Element",
  model: "Model",
  quality: "Quality",
};

// The toggle command behind each side panel.
const PANEL_TOGGLES = {
  outliner: "toggle-outliner",
  inspector: "toggle-inspector",
  editor: "toggle-editor",
  session: "toggle-session",
};

const THEME_MODES = ["system", "light", "dark"];
const THEME_LABELS = { system: "System", light: "Light", dark: "Dark" };
const CANVAS_MODES = ["match", "light", "dark"];
const CANVAS_LABELS = { match: "Match theme", light: "Light", dark: "Dark" };

const TOAST_ICONS = {
  success: '<path d="m5 12 4 4L19 6" />',
  error: '<circle cx="12" cy="12" r="9" /><path d="M12 8v5M12 16h.01" />',
  info: '<circle cx="12" cy="12" r="9" /><path d="M12 11v5M12 8h.01" />',
};

export function createShell() {
  const commands = new Map();
  const buttons = new Map();
  const listeners = { theme: [], canvasTheme: [], panel: [], resize: [], visibility: [] };
  const narrowScreen = matchMedia("(max-width: 860px)");

  let ribbonTab = "home";
  let inspectorPanel = "properties";
  let paletteIndex = 0;
  let paletteItems = [];
  let themeMode = readMode("tessifc.theme", THEME_MODES, "system");
  let canvasMode = readMode("tessifc.canvas", CANVAS_MODES, "match");
  let uiTheme = themeMode === "system" ? systemTheme() : themeMode;
  let canvasTheme = canvasMode === "match" ? uiTheme : canvasMode;

  function bind(element, command) {
    const group = buttons.get(command) ?? [];
    group.push(element);
    buttons.set(command, group);
    element.addEventListener("click", () => run(command));
  }
  for (const [id, command] of Object.entries(BUTTON_COMMANDS)) {
    const element = $(id);
    if (element) bind(element, command);
  }
  // Standard view buttons name their view as data, one command each.
  for (const element of document.querySelectorAll("[data-view]")) {
    bind(element, `view:${element.dataset.view}`);
  }
  // The close buttons reach their panel directly, whatever the command table says.
  $("editor-close")?.addEventListener("click", () => setPanel("editor", false));
  $("session-close")?.addEventListener("click", () => setPanel("session", false));

  /** Register a command so buttons, the palette and the keyboard can run it. */
  function register(list) {
    for (const command of list) commands.set(command.id, command);
  }

  /** Run a command by id. Disabled commands are ignored. */
  function run(id) {
    const command = commands.get(id);
    if (!command || command.disabled) return;
    command.run();
  }

  /** Enable or disable every button bound to a command. */
  function setEnabled(id, enabled) {
    const command = commands.get(id);
    if (command) command.disabled = !enabled;
    for (const element of buttons.get(id) ?? []) element.disabled = !enabled;
  }

  /** Set the pressed state of a toggle command on every button declared as a toggle. */
  function setPressed(id, pressed) {
    for (const element of buttons.get(id) ?? []) {
      if (element.hasAttribute("aria-pressed")) element.setAttribute("aria-pressed", String(pressed));
      if (element.classList.contains("rail-btn")) element.classList.toggle("active", pressed);
    }
  }

  /** Replace the visible label of a command, keeping icons untouched. */
  function setLabel(id, text, title) {
    for (const element of buttons.get(id) ?? []) {
      const label = element.querySelector(".rib-label");
      if (label) label.textContent = text;
      if (title) element.title = title;
    }
  }

  /** Mark a command as carrying unsaved work. */
  function setDirty(id, dirty) {
    for (const element of buttons.get(id) ?? []) element.classList.toggle("dirty", dirty);
  }

  // ------------------------------------------------------------- ribbon

  function setRibbonTab(name) {
    ribbonTab = name;
    for (const tab of document.querySelectorAll(".rib-tab")) {
      const active = tab.dataset.tab === name;
      tab.classList.toggle("active", active);
      tab.setAttribute("aria-selected", String(active));
      tab.tabIndex = active ? 0 : -1;
    }
    for (const strip of document.querySelectorAll(".rib-strip")) {
      strip.classList.toggle("active", strip.id === `rib-${name}`);
    }
  }

  function toggleRibbon(force) {
    const collapsed = force ?? !document.body.classList.contains("ribbon-collapsed");
    document.body.classList.toggle("ribbon-collapsed", collapsed);
    const toggle = $("ribbon-toggle");
    toggle.setAttribute("aria-expanded", String(!collapsed));
    toggle.title = collapsed ? "Expand the ribbon (Ctrl F1)" : "Collapse the ribbon (Ctrl F1)";
    emit("resize");
  }

  for (const tab of document.querySelectorAll(".rib-tab")) {
    tab.addEventListener("click", () => setRibbonTab(tab.dataset.tab));
  }
  bindRovingTabs([...document.querySelectorAll(".rib-tab")], (tab) => setRibbonTab(tab.dataset.tab));
  $("ribbon-toggle").addEventListener("click", () => toggleRibbon());

  // ------------------------------------------------------------- panels

  function panelVisible(name) {
    return !$(name).classList.contains("collapsed");
  }

  function setPanel(name, visible) {
    if (visible && narrowScreen.matches) {
      for (const other of Object.keys(PANEL_TOGGLES)) {
        if (other !== name && panelVisible(other)) setPanel(other, false);
      }
    }
    const panel = $(name);
    // A panel already in the asked state costs nothing, so a selection cannot trigger a resize.
    if (panel.classList.contains("collapsed") === !visible) {
      setPressed(PANEL_TOGGLES[name], visible);
      return;
    }
    // Hand focus to the viewport before collapsing the panel that holds it.
    if (!visible && panel.contains(document.activeElement)) {
      $("viewport").focus({ preventScroll: true });
    }
    panel.classList.toggle("collapsed", !visible);
    if (name === "inspector") $("rail").classList.toggle("closed", !visible);
    setPressed(PANEL_TOGGLES[name], visible);
    emit("visibility", { name, visible });
    emit("resize");
  }

  function togglePanel(name) {
    setPanel(name, !panelVisible(name));
  }

  function collapseNarrowPanels() {
    if (narrowScreen.matches) {
      for (const name of Object.keys(PANEL_TOGGLES)) setPanel(name, false);
    }
  }
  narrowScreen.addEventListener("change", collapseNarrowPanels);
  collapseNarrowPanels();

  function setInspectorPanel(name) {
    inspectorPanel = name;
    for (const button of document.querySelectorAll("#rail .rail-btn[data-panel]")) {
      const active = button.dataset.panel === name;
      button.classList.toggle("active", active);
      button.setAttribute("aria-selected", String(active));
      button.tabIndex = active ? 0 : -1;
    }
    for (const body of document.querySelectorAll("#inspector .panel-body")) {
      body.classList.toggle("hidden", body.id !== `tab-${name}`);
    }
    $("inspector-title").textContent = PANEL_TITLES[name] ?? name;
    setPanel("inspector", true);
    emit("panel", name);
  }

  for (const button of document.querySelectorAll("#rail .rail-btn[data-panel]")) {
    button.addEventListener("click", () => {
      // The open tab folds the inspector away again.
      if (button.dataset.panel === inspectorPanel && panelVisible("inspector")) setPanel("inspector", false);
      else setInspectorPanel(button.dataset.panel);
    });
  }
  bindRovingTabs(
    [...document.querySelectorAll("#rail .rail-btn[data-panel]")],
    (button) => setInspectorPanel(button.dataset.panel),
  );

  const outlinerSwitch = $("outliner-switch");
  function setOutlinerView(name) {
    for (const button of outlinerSwitch.querySelectorAll("button")) {
      const active = button.dataset.outliner === name;
      button.setAttribute("aria-selected", String(active));
      button.tabIndex = active ? 0 : -1;
    }
    $("tree-spatial").classList.toggle("hidden", name !== "spatial");
    $("tree-types").classList.toggle("hidden", name !== "types");
    emit("panel", `outliner:${name}`);
  }
  for (const button of outlinerSwitch.querySelectorAll("button")) {
    button.addEventListener("click", () => setOutlinerView(button.dataset.outliner));
  }
  bindRovingTabs(
    [...outlinerSwitch.querySelectorAll("button")],
    (button) => setOutlinerView(button.dataset.outliner),
  );

  // -------------------------------------------------------------- theme

  // One theme drives the panels and the canvas. The canvas may opt out of it,
  // which is why the two are separate settings rather than one.

  /** Set the interface theme to "system", "light" or "dark". */
  function applyTheme(mode) {
    themeMode = THEME_MODES.includes(mode) ? mode : "system";
    uiTheme = themeMode === "system" ? systemTheme() : themeMode;
    const root = document.documentElement;
    root.dataset.theme = uiTheme;
    root.dataset.themeMode = themeMode;
    document.querySelector('meta[name="theme-color"]')?.setAttribute("content", uiTheme === "light" ? "#f6f7f9" : "#0b0c0e");
    store("tessifc.theme", themeMode);
    setLabel("theme", THEME_LABELS[themeMode], `Interface theme: ${THEME_LABELS[themeMode].toLowerCase()}. Click for the next one.`);
    const select = $("set-theme");
    if (select) select.value = themeMode;
    emit("theme", uiTheme);
    applyCanvasTheme(canvasMode);
  }

  /** Set the viewport background to "match", "light" or "dark". */
  function applyCanvasTheme(mode) {
    canvasMode = CANVAS_MODES.includes(mode) ? mode : "match";
    canvasTheme = canvasMode === "match" ? uiTheme : canvasMode;
    store("tessifc.canvas", canvasMode);
    setLabel("canvas-theme", CANVAS_LABELS[canvasMode], `Viewport background: ${CANVAS_LABELS[canvasMode].toLowerCase()}. Click for the next one.`);
    const select = $("set-canvas");
    if (select) select.value = canvasMode;
    emit("canvasTheme", canvasTheme);
  }

  // A system theme follows the operating system while the page is open.
  matchMedia("(prefers-color-scheme: light)").addEventListener("change", () => {
    if (themeMode === "system") applyTheme("system");
  });

  // ------------------------------------------------------------- toasts

  function toast(message, kind = "info", timeout = 5200) {
    // Kernel messages can quote raw file bytes: bound and strip control characters.
    const safe = Array.from(String(message))
      .filter((character) => {
        const code = character.codePointAt(0);
        return code >= 32 && code !== 127;
      })
      .join("")
      .slice(0, 300);
    const node = document.createElement("div");
    node.className = `toast ${kind}`;
    node.setAttribute("role", kind === "error" ? "alert" : "status");
    node.innerHTML =
      `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" ` +
      `stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${TOAST_ICONS[kind] ?? TOAST_ICONS.info}</svg>`;
    const text = document.createElement("span");
    text.className = "msg";
    text.textContent = safe;
    const close = document.createElement("button");
    close.className = "icon-btn sm";
    close.type = "button";
    close.setAttribute("aria-label", "Dismiss");
    close.textContent = "×";
    close.addEventListener("click", () => dismiss());
    node.append(text, close);
    $("toasts").append(node);

    let closed = false;
    const dismiss = () => {
      if (closed) return;
      closed = true;
      node.classList.add("closing");
      node.addEventListener("animationend", () => node.remove(), { once: true });
      setTimeout(() => node.remove(), 400);
    };
    if (timeout) setTimeout(dismiss, timeout);
    return dismiss;
  }

  // ------------------------------------------------------------ dialogs

  function bindDialog(id, closeIds) {
    const dialog = $(id);
    for (const closeId of closeIds) $(closeId)?.addEventListener("click", () => dialog.close());
    dialog.addEventListener("click", (event) => {
      if (event.target === dialog) dialog.close();
    });
    return dialog;
  }

  const settingsDialog = bindDialog("settings-dialog", ["settings-close", "settings-done"]);
  const helpDialog = bindDialog("help-dialog", ["help-close", "help-done"]);

  function openDialog(dialog) {
    if (!dialog.open) dialog.showModal();
  }

  // ------------------------------------------------------------ palette

  const paletteBackdrop = $("palette-backdrop");
  const paletteInput = $("palette-input");
  const paletteList = $("palette-list");

  let paletteReturnFocus = null;

  function openPalette() {
    paletteReturnFocus = document.activeElement;
    paletteBackdrop.classList.remove("hidden");
    paletteInput.value = "";
    renderPalette("");
    paletteInput.focus();
  }

  /** Close and hand focus back to where it was, so the keyboard does not land on the body. */
  function closePalette() {
    if (paletteBackdrop.classList.contains("hidden")) return;
    paletteBackdrop.classList.add("hidden");
    const target = paletteReturnFocus;
    paletteReturnFocus = null;
    if (target instanceof HTMLElement && target.isConnected) target.focus();
  }

  function renderPalette(query) {
    const needle = query.trim().toLowerCase();
    paletteItems = [...commands.values()]
      .filter((command) => command.palette !== false && !command.disabled)
      .filter((command) => !needle || `${command.label} ${command.section ?? ""}`.toLowerCase().includes(needle))
      .slice(0, 60);
    paletteIndex = 0;
    paletteList.replaceChildren(
      ...paletteItems.map((command, index) => {
        const button = document.createElement("button");
        button.type = "button";
        button.className = `pal-item${index === 0 ? " active" : ""}`;
        const label = document.createElement("span");
        label.className = "grow";
        label.textContent = command.label;
        const section = document.createElement("span");
        section.className = "sec";
        section.textContent = command.section ?? "";
        button.append(label, section);
        if (command.hint) {
          const key = document.createElement("kbd");
          key.textContent = command.hint;
          button.append(key);
        }
        button.addEventListener("mousemove", () => movePalette(index - paletteIndex));
        button.addEventListener("click", () => {
          closePalette();
          command.run();
        });
        return button;
      }),
    );
    if (!paletteItems.length) {
      const empty = document.createElement("p");
      empty.className = "pal-empty";
      empty.textContent = "No command matches that search.";
      paletteList.replaceChildren(empty);
    }
  }

  function movePalette(delta) {
    if (!paletteItems.length) return;
    const nodes = [...paletteList.children];
    nodes[paletteIndex]?.classList.remove("active");
    paletteIndex = (paletteIndex + delta + paletteItems.length) % paletteItems.length;
    nodes[paletteIndex]?.classList.add("active");
    nodes[paletteIndex]?.scrollIntoView({ block: "nearest" });
  }

  paletteInput.addEventListener("input", () => renderPalette(paletteInput.value));
  paletteInput.addEventListener("keydown", (event) => {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      movePalette(1);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      movePalette(-1);
    } else if (event.key === "Enter") {
      event.preventDefault();
      const command = paletteItems[paletteIndex];
      closePalette();
      command?.run();
    } else if (event.key === "Escape") {
      event.preventDefault();
      closePalette();
    }
  });
  paletteBackdrop.addEventListener("click", (event) => {
    if (event.target === paletteBackdrop) closePalette();
  });
  // Escape works wherever focus sits inside the palette, and Tab stays inside it.
  paletteBackdrop.addEventListener("keydown", (event) => {
    if (event.key === "Escape") {
      event.preventDefault();
      closePalette();
      return;
    }
    if (event.key !== "Tab") return;
    const stops = [paletteInput, ...paletteList.querySelectorAll("button")];
    const index = stops.indexOf(document.activeElement);
    const next = stops[(index + (event.shiftKey ? -1 : 1) + stops.length) % stops.length];
    event.preventDefault();
    next?.focus();
  });

  // ---------------------------------------------------------- shortcuts

  let onEscape = () => {};

  window.addEventListener("keydown", (event) => {
    // An open modal owns the keyboard, including Escape and the palette.
    if (settingsDialog.open || helpDialog.open) return;
    const target = event.target;
    const typing =
      target instanceof HTMLInputElement ||
      target instanceof HTMLTextAreaElement ||
      target instanceof HTMLSelectElement ||
      target?.isContentEditable;
    const control = event.ctrlKey || event.metaKey;
    const key = event.key;
    const lower = typeof key === "string" ? key.toLowerCase() : "";

    if (control && lower === "k") {
      event.preventDefault();
      paletteBackdrop.classList.contains("hidden") ? openPalette() : closePalette();
      return;
    }
    if (!paletteBackdrop.classList.contains("hidden")) return;

    if (control && lower === "o") {
      event.preventDefault();
      run("open");
      return;
    }
    if (control && lower === "s") {
      event.preventDefault();
      run("export");
      return;
    }
    if (control && lower === "b") {
      event.preventDefault();
      run("toggle-outliner");
      return;
    }
    if (control && key === ",") {
      event.preventDefault();
      run("settings");
      return;
    }
    if (control && key === "F1") {
      event.preventDefault();
      toggleRibbon();
      return;
    }
    if (control && lower === "f" && !typing) {
      event.preventDefault();
      setPanel("outliner", true);
      $("tree-search").focus();
      $("tree-search").select();
      return;
    }

    if (key === "Escape") {
      if (typing) {
        target.blur();
        return;
      }
      onEscape();
      return;
    }
    if (typing || control || event.altKey) return;

    if (key === "?" || (event.shiftKey && key === "/")) {
      event.preventDefault();
      run("help");
      return;
    }
    if (key === "\\") {
      event.preventDefault();
      run("toggle-inspector");
      return;
    }

    const single = {
      "+": "zoom-in",
      "=": "zoom-in",
      "-": "zoom-out",
      f: event.shiftKey ? "focus" : "fit",
      p: "plan",
      d: "style",
      m: "measure",
      x: "section",
      i: "isolate",
      h: "hide",
      a: "show-all",
      e: "toggle-editor",
      1: "view:front",
      2: "view:right",
      3: "view:top",
      4: "view:perspective",
    }[lower];
    if (single && commands.has(single)) {
      event.preventDefault();
      run(single);
    }
  });

  // ------------------------------------------------------------- events

  function on(name, handler) {
    listeners[name].push(handler);
  }

  function emit(name, value) {
    for (const handler of listeners[name]) handler(value);
  }

  return {
    register,
    run,
    setEnabled,
    setPressed,
    setLabel,
    setDirty,
    setRibbonTab,
    toggleRibbon,
    setPanel,
    togglePanel,
    panelVisible,
    setInspectorPanel,
    setOutlinerView,
    applyTheme,
    applyCanvasTheme,
    cycleTheme: () => applyTheme(THEME_MODES[(THEME_MODES.indexOf(themeMode) + 1) % THEME_MODES.length]),
    cycleCanvasTheme: () => applyCanvasTheme(CANVAS_MODES[(CANVAS_MODES.indexOf(canvasMode) + 1) % CANVAS_MODES.length]),
    themeMode: () => themeMode,
    canvasMode: () => canvasMode,
    uiTheme: () => uiTheme,
    canvasTheme: () => canvasTheme,
    ribbonTab: () => ribbonTab,
    inspectorPanel: () => inspectorPanel,
    toast,
    openSettings: () => openDialog(settingsDialog),
    openHelp: () => openDialog(helpDialog),
    openPalette,
    closePalette,
    setEscapeHandler: (handler) => (onEscape = handler),
    on,
  };
}

/** Arrow-key movement inside one tab list. */
function bindRovingTabs(tabs, activate) {
  const steps = { ArrowLeft: -1, ArrowUp: -1, ArrowRight: 1, ArrowDown: 1 };
  for (const [index, tab] of tabs.entries()) {
    tab.addEventListener("keydown", (event) => {
      let next = index;
      if (event.key in steps) next = (index + steps[event.key] + tabs.length) % tabs.length;
      else if (event.key === "Home") next = 0;
      else if (event.key === "End") next = tabs.length - 1;
      else return;
      event.preventDefault();
      tabs[next].focus();
      activate(tabs[next]);
    });
  }
}

function readStored(key) {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

/** A stored setting, or the fallback when it is absent or no longer valid. */
function readMode(key, allowed, fallback) {
  const value = readStored(key);
  return allowed.includes(value) ? value : fallback;
}

function systemTheme() {
  return matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark";
}

function store(key, value) {
  try {
    localStorage.setItem(key, value);
  } catch {
    // A blocked storage backend must not break the interface.
  }
}
