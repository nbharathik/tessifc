// SPDX-License-Identifier: Apache-2.0

/** Exercise selection identity and queued replies without depending on worker timing. */
export async function checkSelectionUpdates(page, check) {
  const result = await page.evaluate(() => {
    const T = window.__tessifc;
    for (const name of ["selectRecord", "selectExpressId", "receiveEntityInfo", "receiveEntityError", "runPanelWork"]) {
      if (typeof T[name] !== "function") throw new Error(`The test harness must expose ${name}`);
    }
    if (!T.panelWork || !T.inspector || !T.shell) throw new Error("The test harness must expose panelWork, inspector and shell");
    const entries = [...T.state.model.index.recordsByExpressId].filter(([, records]) => records.length).slice(0, 2);
    if (entries.length !== 2) throw new Error("The selection fixture requires two products");
    const [[idA, recordsA], [idB, recordsB]] = entries;
    const saved = { selection: T.state.selection, worker: T.state.worker, model: T.state.model,
      panel: { ...T.panelWork }, dockHidden: document.getElementById("dock-selection").classList.contains("hidden") };
    const patches = [], requests = [], shown = [], properties = [], selected = [], revealed = [], panels = [];
    let visible = true, panel = "properties";
    const replace = (object, key, value) => { patches.push([object, key, object[key]]); object[key] = value; };
    const flush = () => {
      if (T.panelWork.frame) cancelAnimationFrame(T.panelWork.frame);
      T.runPanelWork();
    };
    const reply = (selection, info) => T.receiveEntityInfo({ requestId: selection.requestId, expressId: selection.expressId, info });
    try {
      if (T.panelWork.frame) cancelAnimationFrame(T.panelWork.frame);
      Object.assign(T.panelWork, { selection: false, visibility: false, properties: undefined, frame: 0 });
      T.state.worker = { postMessage(message) { requests.push(message); } };
      replace(T.renderer, "select", (records) => selected.push(records));
      replace(T.tree, "select", (expressId) => revealed.push(expressId));
      replace(T.inspector, "showSelection", (selection) => shown.push(selection));
      replace(T.inspector, "setProperties", (info) => properties.push(info));
      replace(T.inspector, "setPropertyError", () => {});
      replace(T.shell, "panelVisible", () => visible);
      replace(T.shell, "inspectorPanel", () => panel);
      replace(T.shell, "setPanel", (name, value) => { visible = value; panels.push(name); });
      replace(T.shell, "setInspectorPanel", (name) => { panel = name; visible = true; panels.push(name); });
      for (const name of ["setEnabled", "setPressed", "setLabel"]) replace(T.shell, name, () => {});

      T.selectRecord(recordsA[0], true);
      const first = T.state.selection;
      T.selectRecord(recordsA[0]); T.selectRecord(recordsA[0]);
      flush();
      const pendingStable = T.state.selection === first && requests.length === 1 && shown.length === 1
        && selected.length === 1 && revealed.length === 1;

      reply(first, { expressId: idA, fields: [] });
      T.selectRecord(recordsA[0]);
      flush();
      const readyStable = T.state.selection === first && requests.length === 1 && shown.length === 1
        && selected.length === 1 && revealed.length === 1 && properties.length === 1;

      visible = false;
      const panelsBefore = panels.length;
      T.selectRecord(recordsA[0]);
      const reopened = visible && panels.length === panelsBefore + 1 && requests.length === 1 && shown.length === 1;

      T.receiveEntityError({ requestId: first.requestId, expressId: idA, message: "Retry the fixture reply" });
      T.selectRecord(recordsA[0]);
      const retried = T.state.selection !== first && requests.length === 2 && T.state.selection.infoState === "pending";
      const selectionA = T.state.selection;
      reply(selectionA, { expressId: idA, fields: [] });
      T.selectRecord(recordsB[0]);
      const selectionB = T.state.selection;
      const beforeStale = properties.length;
      flush();
      const queuedStaleIgnored = properties.length === beforeStale && T.state.selection === selectionB && revealed.at(-1) === idB;
      reply(selectionA, { expressId: idA, fields: [] });
      flush();
      const lateStaleIgnored = properties.length === beforeStale && !selectionB.info;

      reply(selectionB, { expressId: idB, fields: [] });
      flush();
      const latestApplied = properties.at(-1)?.expressId === idB && selectionB.infoState === "ready";
      reply(selectionB, { expressId: idB, fields: [], revision: "before-edit" });
      const beforeEdit = properties.length;
      selectionB.info = { expressId: idB, fields: [], revision: "edited" };
      flush();
      const editPreserved = properties.length === beforeEdit && selectionB.info.revision === "edited";

      const beforeRefresh = requests.length;
      T.selectExpressId(idB, true);
      const refreshed = requests.length === beforeRefresh + 1 && T.state.selection !== selectionB
        && T.state.selection.requestId !== selectionB.requestId;
      const beforeModel = requests.length;
      T.state.model = { ...saved.model, modelId: saved.model.modelId + 1 };
      T.selectRecord(recordsB[0]);
      const modelRefreshed = requests.length === beforeModel + 1 && T.state.selection.modelId === T.state.model.modelId;
      return { pendingStable, readyStable, reopened, retried, queuedStaleIgnored, lateStaleIgnored, latestApplied, editPreserved, refreshed, modelRefreshed };
    } finally {
      if (T.panelWork.frame) cancelAnimationFrame(T.panelWork.frame);
      for (const [object, key, original] of patches.reverse()) object[key] = original;
      T.state.selection = saved.selection; T.state.worker = saved.worker; T.state.model = saved.model;
      Object.assign(T.panelWork, saved.panel, { frame: 0 });
      document.getElementById("dock-selection").classList.toggle("hidden", saved.dockHidden);
      T.runPanelWork();
    }
  });
  check(result.pendingStable, "repeated selection keeps one pending request, highlight and tree reveal");
  check(result.readyStable, "repeated selection preserves loaded properties without rebuilding its UI");
  check(result.reopened, "reselecting an element reopens its inspector without requesting its properties again");
  check(result.retried, "reselecting after a property error starts a fresh request");
  check(result.queuedStaleIgnored && result.lateStaleIgnored, "quick selection changes reject both queued and late properties for the earlier element");
  check(result.latestApplied, "the current element receives its own properties");
  check(result.editPreserved, "queued properties cannot replace newer edit data for the same element");
  check(result.refreshed && result.modelRefreshed, "explicit geometry refresh and a different model renew the selection request");
}
