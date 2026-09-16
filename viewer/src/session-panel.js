// SPDX-License-Identifier: Apache-2.0

//! The session panel: a script editor and an assistant over the open model.
//! Scripts run in the browser or, with a local session connected, on its host;
//! both publish revisions through the same snapshot path.

import { duration } from "./format.js";
import { JAVASCRIPT_EXAMPLES, PYTHON_EXAMPLES } from "./script-examples.js";

const $ = (id) => document.getElementById(id);

const SESSION_COMMAND = "python scripts/serve-edit-session.py model.ifc";

export function createSessionPanel({ shell, getSession, getSelection, browser, assistant, hasModel }) {
  const editor = $("script-editor");
  const output = $("script-output");
  const log = $("assistant-log");
  const prompt = $("assistant-prompt");
  const examples = $("script-example");
  let tab = "script";
  let mode = "ask";
  let status = null;
  let busy = false;
  let hasSelection = false;
  let engineKind = null;
  const history = [];

  $("session-command").textContent = SESSION_COMMAND;

  // ------------------------------------------------------------ engines

  /** The engine behind the panel: the connected session (Python or JavaScript) when there is one, the browser otherwise. */
  function engine() {
    const session = getSession();
    if (session && status) {
      const capabilities = status.capabilities ?? {};
      // Older hosts report a boolean; a string names the language the session runs.
      const language = capabilities.authoring === "javascript" ? "javascript" : capabilities.authoring ? "python" : null;
      const label = language === "javascript"
        ? `JavaScript · ${capabilities.engine ?? "session"}`
        : `Python · ${language ? `IfcOpenShell ${capabilities.ifcopenshell ?? ""}`.trim() : "scripts unavailable"}`;
      return {
        kind: "session",
        language: language ?? "python",
        label,
        ready: Boolean(language) && hasModel(),
        note: language ? "" : capabilities.authoringError ?? "Install IfcOpenShell in the session's Python environment.",
        undoCount: status.undo ?? 0,
        redoCount: status.redo ?? 0,
        run: (script, selection) => session.run(script, selection),
        undo: () => session.undo(),
        redo: () => session.redo(),
        applied: (result) => session.whenApplied(result.version, result.marker),
        assistant: capabilities.assistant ? { describe: () => capabilities.assistant, respond: (request) => session.assistant(request) } : null,
        examples: language === "javascript" ? (status.examples?.length ? status.examples : JAVASCRIPT_EXAMPLES) : PYTHON_EXAMPLES,
      };
    }
    return {
      kind: "javascript",
      language: "javascript",
      label: "JavaScript · in this browser",
      ready: hasModel(),
      note: hasModel() ? "" : "Open a model to run scripts.",
      undoCount: browser.history().undo,
      redoCount: browser.history().redo,
      run: (script, selection) => browser.run(script, selection),
      undo: () => browser.undo(),
      redo: () => browser.redo(),
      applied: (result) => Promise.resolve(result.impact ?? null),
      assistant: assistant.configured() ? { describe: () => assistant.describe(), respond: (request) => assistant.respond(request) } : null,
      examples: JAVASCRIPT_EXAMPLES,
    };
  }

  /** Reflect the session status, or the browser engine when `next` is null. */
  function setStatus(next) {
    status = next;
    refresh();
  }

  function refresh() {
    const current = engine();
    const provider = current.assistant?.describe();
    const parts = [current.label, provider ? `assistant ${provider.model ?? provider.provider}` : "assistant off"];
    if (status?.name) parts.unshift(status.name);
    $("session-status").textContent = parts.join(" · ");
    $("session-dot").className = `dot ${busy || status?.busy ? "busy" : current.ready ? "on" : ""}`.trim();
    $("script-note").textContent = current.note;
    $("session-python-hint").classList.toggle("hidden", current.kind === "session");
    $("assistant-note").textContent = current.assistant
      ? ""
      : current.kind === "session"
        ? current.language === "javascript"
          ? "This session is driven by an MCP client; the browser assistant works on a locally opened file."
          : "Start the session with an assistant provider to ask questions or request edits."
        : "Choose an assistant provider under Settings to ask questions or request edits.";
    const engineId = `${current.kind}:${current.language}`;
    if (engineId !== engineKind) {
      engineKind = engineId;
      fillExamples(current.examples);
      $("script-language").textContent = current.language === "python" ? "Python" : "JavaScript";
      editor.placeholder = current.language === "python"
        ? "wall = selected or model.by_type('IfcWall')[0]\nprint(wall.Name)"
        : 'const wall = selected ?? ifc.byType("IfcWall")[0];\nprint(wall.Name);';
    }
    const ready = current.ready && !busy;
    $("script-run").disabled = !ready;
    $("script-undo").disabled = !ready || current.undoCount === 0;
    $("script-redo").disabled = !ready || current.redoCount === 0;
    $("script-insert").disabled = !hasSelection;
    examples.disabled = !current.examples.length;
    const askable = Boolean(current.assistant) && hasModel();
    $("assistant-send").disabled = !askable || busy;
    prompt.disabled = !askable;
  }

  function fillExamples(list) {
    examples.replaceChildren(new Option("Examples", "", true, true));
    list.forEach((example, index) => examples.append(new Option(example.title, String(index))));
    examples.selectedIndex = 0;
  }

  examples.addEventListener("change", () => {
    const example = engine().examples[Number(examples.value)];
    if (example) {
      editor.value = example.source;
      editor.focus();
    }
    examples.selectedIndex = 0;
  });

  function setBusy(value) {
    busy = value;
    refresh();
  }

  function setSelection(present) {
    hasSelection = present;
    $("script-insert").disabled = !present;
  }

  // ------------------------------------------------------------- panel

  function setTab(name) {
    tab = name;
    for (const button of $("session-switch").querySelectorAll("button")) {
      const active = button.dataset.session === name;
      button.setAttribute("aria-selected", String(active));
      button.tabIndex = active ? 0 : -1;
    }
    $("session-script").classList.toggle("hidden", name !== "script");
    $("session-assistant").classList.toggle("hidden", name !== "assistant");
    syncRail();
  }

  function syncRail() {
    const visible = shell.panelVisible("session");
    for (const [id, name] of [["rail-script", "script"], ["rail-assistant", "assistant"]]) {
      const active = visible && tab === name;
      $(id).classList.toggle("active", active);
      $(id).setAttribute("aria-pressed", String(active));
    }
  }

  /** Open the panel on a tab, or fold it when that tab is already showing. */
  function open(name) {
    if (shell.panelVisible("session") && tab === name) {
      shell.setPanel("session", false);
      return;
    }
    setTab(name);
    shell.setPanel("session", true);
    refresh();
    (name === "script" ? editor : prompt).focus({ preventScroll: true });
  }

  for (const button of $("session-switch").querySelectorAll("button")) {
    button.addEventListener("click", () => setTab(button.dataset.session));
  }
  shell.on("visibility", ({ name }) => {
    if (name === "session") syncRail();
  });

  // ------------------------------------------------------------ script

  function selectionPayload() {
    const selection = getSelection();
    if (!selection) return null;
    return { ids: [selection.expressId], guids: selection.globalId ? [selection.globalId] : [] };
  }

  function appendOutput(text, kind = "") {
    const line = document.createElement("div");
    line.className = `out-line ${kind}`.trim();
    line.textContent = text;
    output.append(line);
    output.classList.remove("hidden");
    output.scrollTop = output.scrollHeight;
    while (output.childElementCount > 200) output.firstElementChild.remove();
  }

  function describeRun(result) {
    if (!result.ok) {
      appendOutput(`${result.error}\n${result.traceback ?? ""}`.trim(), "error");
      if (result.stdout) appendOutput(result.stdout.trimEnd());
      return;
    }
    if (result.stdout) appendOutput(result.stdout.trimEnd());
    const ops = result.operations ?? {};
    const journal = ["created", "modified", "deleted"].filter((key) => ops[key]).map((key) => `${ops[key]} ${key}`).join(", ");
    const elapsed = duration(result.elapsedMs ?? 0);
    appendOutput(result.changed
      ? `Saved in ${elapsed}${journal ? ` (${journal})` : ""}; updating the view`
      : `No changes in ${elapsed}`, "meta");
  }

  function describeImpact(impact) {
    if (!impact) {
      appendOutput("View updated", "meta");
      return;
    }
    const label = impact.fullRebuild
      ? "full geometry refresh"
      : `${impact.affectedProducts?.length ?? 0} geometry updates, ${impact.removedProducts?.length ?? 0} removed`;
    const timing = impact.workerMs != null ? ` in ${duration(impact.workerMs)}` : "";
    const revision = impact.revision != null ? `revision ${impact.revision}, ` : "";
    appendOutput(`View updated: ${revision}${label}${timing}`, "meta");
  }

  async function reportApplied(current, result) {
    if (!result.ok || !result.changed) return;
    try {
      describeImpact(await current.applied(result));
    } catch (error) {
      appendOutput(`The viewer did not apply this revision: ${error.message}`, "error");
    }
  }

  async function perform(action) {
    if (busy) return undefined;
    const current = engine();
    setBusy(true);
    try {
      const result = await action(current);
      describeRun(result);
      await reportApplied(current, result);
      return result;
    } catch (error) {
      if (error.script) describeRun(error.script);
      appendOutput(error.message, "error");
      return { ok: false, error: error.message };
    } finally {
      setBusy(false);
    }
  }

  function runScript(script = editor.value) {
    if (!script.trim()) {
      appendOutput("Type a script first, or pick an example.", "error");
      return Promise.resolve({ ok: false });
    }
    appendOutput(`> run (${script.split("\n").length} lines)`, "prompt");
    return perform((current) => current.run(script, selectionPayload()));
  }

  $("script-run").addEventListener("click", () => runScript());
  $("script-undo").addEventListener("click", () => {
    appendOutput("> undo", "prompt");
    perform((current) => current.undo());
  });
  $("script-redo").addEventListener("click", () => {
    appendOutput("> redo", "prompt");
    perform((current) => current.redo());
  });
  $("script-clear").addEventListener("click", () => {
    output.replaceChildren();
    output.classList.add("hidden");
  });
  $("script-insert").addEventListener("click", () => {
    const selection = getSelection();
    if (!selection) return;
    const python = engine().language === "python";
    const target = selection.globalId
      ? `model.by_guid(${JSON.stringify(selection.globalId)})`
      : `model.by_id(${selection.expressId})`;
    const browserTarget = selection.globalId ? `ifc.byGuid(${JSON.stringify(selection.globalId)})` : `ifc.get(${selection.expressId})`;
    const label = selection.name ? ` ${JSON.stringify(selection.name)}` : "";
    insertAtCursor(python
      ? `target = ${target}  # ${selection.className}${label}\n`
      : `const target = ${browserTarget};  // ${selection.className}${label}\n`);
  });

  function insertAtCursor(text) {
    const { selectionStart: start, selectionEnd: end, value } = editor;
    const lineStart = start === 0 || value[start - 1] === "\n" ? "" : "\n";
    editor.setRangeText(lineStart + text, start, end, "end");
    editor.focus();
  }

  editor.addEventListener("keydown", (event) => {
    if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
      event.preventDefault();
      runScript();
    } else if (event.key === "Tab" && !event.shiftKey) {
      event.preventDefault();
      editor.setRangeText("  ", editor.selectionStart, editor.selectionEnd, "end");
    }
  });

  // --------------------------------------------------------- assistant

  function setMode(name) {
    mode = name;
    for (const button of $("assistant-mode").querySelectorAll("button")) {
      const active = button.dataset.mode === name;
      button.setAttribute("aria-selected", String(active));
      button.tabIndex = active ? 0 : -1;
    }
    $("assistant-policy").classList.toggle("hidden", name !== "edit");
    prompt.placeholder = name === "edit"
      ? "Describe the change, for example: add a door to the selected wall"
      : "Ask about the model, for example: how many walls are there?";
  }
  for (const button of $("assistant-mode").querySelectorAll("button")) {
    button.addEventListener("click", () => setMode(button.dataset.mode));
  }

  function addMessage(role, text) {
    const node = document.createElement("div");
    node.className = `chat-msg ${role}`;
    node.textContent = text;
    log.append(node);
    log.scrollTop = log.scrollHeight;
    return node;
  }

  function addProposal(proposal, run) {
    const card = document.createElement("div");
    card.className = "proposal";
    const head = document.createElement("div");
    head.className = "proposal-head";
    head.textContent = proposal.summary || "Proposed script";
    const code = document.createElement("pre");
    code.className = "proposal-code";
    code.textContent = proposal.script;
    const actions = document.createElement("div");
    actions.className = "edit-actions";
    const runButton = document.createElement("button");
    runButton.type = "button";
    runButton.className = "btn accent sm";
    runButton.textContent = run ? (run.ok ? "Ran" : "Failed") : "Run";
    runButton.disabled = Boolean(run);
    runButton.addEventListener("click", async () => {
      runButton.disabled = true;
      editor.value = proposal.script;
      setTab("script");
      const result = await runScript(proposal.script);
      runButton.textContent = result?.ok ? "Ran" : "Failed";
      runButton.disabled = Boolean(result?.ok);
    });
    const openButton = document.createElement("button");
    openButton.type = "button";
    openButton.className = "btn sm";
    openButton.textContent = "Open in script";
    openButton.addEventListener("click", () => {
      editor.value = proposal.script;
      setTab("script");
      editor.focus();
    });
    actions.append(runButton, openButton);
    card.append(head, code, actions);
    log.append(card);
    log.scrollTop = log.scrollHeight;
  }

  async function send() {
    const current = engine();
    const text = prompt.value.trim();
    if (!current.assistant || busy || !text) return;
    prompt.value = "";
    addMessage("user", text);
    const pending = addMessage("assistant pending", mode === "edit" ? "Preparing the edit" : "Looking at the model");
    setBusy(true);
    try {
      const policy = $("assistant-auto").checked ? "auto" : "review";
      const response = await current.assistant.respond({
        mode, prompt: text, policy, selection: selectionPayload(), history: history.slice(-20),
      });
      pending.remove();
      history.push({ role: "user", content: text });
      if (response.answer) {
        addMessage("assistant", response.answer);
        history.push({ role: "assistant", content: response.answer });
      }
      if (response.proposal) {
        addProposal(response.proposal, response.run);
        history.push({ role: "assistant", content: `Proposed script:\n${response.proposal.script}` });
      }
      if (response.run) {
        editor.value = response.proposal?.script ?? editor.value;
        appendOutput("> assistant edit", "prompt");
        describeRun(response.run);
        await reportApplied(current, response.run);
      }
    } catch (error) {
      pending.remove();
      addMessage("assistant error", String(error.message).slice(0, 400));
    } finally {
      setBusy(false);
      prompt.focus();
    }
  }

  $("assistant-send").addEventListener("click", send);
  $("assistant-clear").addEventListener("click", () => {
    history.length = 0;
    log.replaceChildren();
  });
  prompt.addEventListener("keydown", (event) => {
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      send();
    }
  });

  setTab("script");
  setMode("ask");
  refresh();

  return { open, setStatus, setSelection, refresh, runScript, tab: () => tab, mode: () => mode, setMode, engine: () => engine().kind, language: () => engine().language };
}
