// SPDX-License-Identifier: Apache-2.0
// @ts-check
import init, { Kernel } from "@tessifc/core/web";
import { createViewer, STYLES } from "@tessifc/viewer";

// `?worker=1` runs the kernel in the package's worker; the page then loads no kernel of its own.
const params = new URLSearchParams(location.search);
const worker = params.get("worker") === "1";
let kernel = null;
if (!worker) {
  await init();
  kernel = new Kernel();
}
const viewer = createViewer(document.getElementById("host"), worker
  ? { worker: { scriptTimeoutMs: Number(params.get("scriptTimeoutMs") ?? 30_000) }, theme: "dark" }
  : { kernel, theme: "dark" });
// Handles for the console and the tests.
Object.assign(window, { tessifcViewer: viewer, tessifcKernel: kernel });

const $ = (id) => document.getElementById(id);
const status = (text) => { $("status").hidden = false; $("status").textContent = text; };

viewer.on("progress", ({ done, total, triangles }) => status(`${done} of ${total} products, ${triangles.toLocaleString()} triangles`));
viewer.on("load", ({ info, summary }) => {
  $("bar").hidden = false;
  status(`${info.schema}: ${summary.products.toLocaleString()} products, ${summary.triangles.toLocaleString()} triangles`);
});
viewer.on("select", (selection) => {
  if (!selection) return;
  const pack = viewer.pack();
  const record = selection.records[0];
  status(`#${selection.expressIds[0]} ${pack.index.classes[pack.instances.classIds[record]]}`);
});
// A followed session publishes revisions; the kernel's counts say what changed.
viewer.on("revision", ({ revision, affectedProducts, removedProducts, fullRebuild }) => {
  status(fullRebuild ? `Revision ${revision}: full refresh` : `Revision ${revision}: ${affectedProducts.length} updated, ${removedProducts.length} removed`);
});
viewer.on("session", ({ error }) => { if (error) status(error); });

function followSession() {
  $("drop").classList.add("hidden");
  status("Following the local session");
  viewer.follow("");
}
$("follow").addEventListener("click", (event) => { event.stopPropagation(); followSession(); });
if (params.get("session") === "file") followSession();

async function openFile(file) {
  $("drop").classList.add("hidden");
  status(`Reading ${file.name}`);
  try {
    await viewer.open(file);
  } catch (error) {
    status(String(error?.message ?? error));
    $("drop").classList.remove("hidden");
  }
}

$("drop").addEventListener("click", () => $("file").click());
$("file").addEventListener("change", (event) => {
  const input = /** @type {HTMLInputElement} */ (event.target);
  if (input.files?.[0]) openFile(input.files[0]);
});
window.addEventListener("dragover", (event) => { event.preventDefault(); $("drop").classList.add("over"); });
window.addEventListener("dragleave", () => $("drop").classList.remove("over"));
window.addEventListener("drop", (event) => {
  event.preventDefault();
  $("drop").classList.remove("over");
  if (event.dataTransfer.files[0]) openFile(event.dataTransfer.files[0]);
});

let style = 0;
let sectioned = false;
$("fit").addEventListener("click", () => viewer.fit());
$("focus").addEventListener("click", () => viewer.focus());
$("top").addEventListener("click", () => viewer.setView("top"));
$("iso").addEventListener("click", () => viewer.setView("perspective"));
$("style").addEventListener("click", () => viewer.setStyle(STYLES[(style = (style + 1) % STYLES.length)]));
$("hide").addEventListener("click", () => viewer.hide(viewer.selection()?.expressIds ?? []));
$("isolate").addEventListener("click", () => viewer.isolate(viewer.selection()?.expressIds ?? null));
$("show-all").addEventListener("click", () => viewer.showAll());
$("section").addEventListener("click", () => viewer.setSection((sectioned = !sectioned) ? { axis: "z", fraction: 0.6 } : null));
