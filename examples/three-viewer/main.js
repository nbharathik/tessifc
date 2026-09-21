// SPDX-License-Identifier: Apache-2.0
// @ts-check
import * as THREE from "three";
import { OrbitControls } from "three/addons/controls/OrbitControls.js";
import init, { Kernel } from "@tessifc/core/web";
import { loadModel, frameCamera, expressIdAt, disposeModel } from "@tessifc/three";

const $ = (id) => document.getElementById(id);

// ------------------------------------------------------------------ the scene

const renderer = new THREE.WebGLRenderer({ antialias: true });
renderer.setPixelRatio(Math.min(devicePixelRatio, 2));
renderer.setSize(innerWidth, innerHeight);
document.body.appendChild(renderer.domElement);

const scene = new THREE.Scene();
scene.background = new THREE.Color(0x1b1d21);

const camera = new THREE.PerspectiveCamera(50, innerWidth / innerHeight, 0.1, 5000);
const controls = new OrbitControls(camera, renderer.domElement);
controls.enableDamping = true;

// IFC is Z-up; a +Z camera up vector keeps the kernel's coordinates as they are.
scene.up.set(0, 0, 1);
camera.up.set(0, 0, 1);

scene.add(new THREE.AmbientLight(0xffffff, 1.4));
const key = new THREE.DirectionalLight(0xffffff, 1.8);
key.position.set(1, 1, 2);
scene.add(key);
const fill = new THREE.DirectionalLight(0xffffff, 0.6);
fill.position.set(-1, -0.5, 0.5);
scene.add(fill);

addEventListener("resize", () => {
  camera.aspect = innerWidth / innerHeight;
  camera.updateProjectionMatrix();
  renderer.setSize(innerWidth, innerHeight);
});

(function frame() {
  requestAnimationFrame(frame);
  controls.update();
  renderer.render(scene, camera);
})();

// ----------------------------------------------------------------- the kernel

await init();
const kernel = new Kernel();
let current = null;
let openRequest = 0;

async function open(file) {
  if (!file.name.toLowerCase().endsWith(".ifc")) {
    fail("Choose a file with the .ifc extension.");
    return;
  }
  try {
    await convert(file);
  } catch (error) {
    fail(error instanceof Error ? error.message : String(error));
  }
}

function fail(message) {
  const note = $("hint");
  note.hidden = false;
  note.textContent = message;
}

async function convert(file) {
  const request = ++openRequest;
  const bytes = new Uint8Array(await file.arrayBuffer());
  if (request !== openRequest) return;
  const startedParse = performance.now();
  const id = kernel.openModel(bytes);
  const parseMs = performance.now() - startedParse;
  const info = JSON.parse(kernel.getModelInfo(id));

  const startedGeometry = performance.now();
  let loaded;
  try {
    loaded = loadModel(THREE, kernel, id);
    if (!loaded.summary.products) throw new Error("No supported geometry. Try another IFC file.");
  } catch (error) {
    if (loaded) disposeModel(loaded.group);
    kernel.closeModel(id);
    throw error;
  }
  const { group, bounds, summary, batches } = loaded;
  const geometryMs = performance.now() - startedGeometry;

  if (current) {
    disposeModel(current.group);
    kernel.closeModel(current.id);
  }
  scene.add(group);
  frameCamera(camera, bounds, controls);
  current = { id, group };

  $("name").textContent = file.name;
  $("schema").textContent = info.schema;
  $("products").textContent = summary.products.toLocaleString();
  $("triangles").textContent = summary.triangles.toLocaleString();
  $("batches").textContent = String(batches);
  $("parse").textContent = `${parseMs.toFixed(0)} ms`;
  $("geometry").textContent = `${geometryMs.toFixed(0)} ms`;
  $("panel").hidden = false;
  $("hint").hidden = false;
  $("hint").textContent = summary.diagnosticErrors || summary.diagnosticWarnings
    ? `${summary.diagnosticErrors} errors, ${summary.diagnosticWarnings} warnings. Inspect outcomes before accepting geometry.`
    : "drag to orbit · scroll to zoom · click an element";
  $("picked").className = "none";
  $("drop").classList.add("hidden");
}

// ------------------------------------------------------------------- the input

const drop = $("drop");
drop.addEventListener("click", () => $("file").click());
$("file").addEventListener("change", (event) => {
  const input = /** @type {HTMLInputElement} */ (event.target);
  if (input.files?.[0]) open(input.files[0]);
  input.value = "";
});
for (const type of ["dragenter", "dragover"]) {
  addEventListener(type, (event) => {
    event.preventDefault();
    drop.classList.add("over");
  });
}
addEventListener("dragleave", () => drop.classList.remove("over"));
addEventListener("drop", (event) => {
  event.preventDefault();
  drop.classList.remove("over");
  const file = event.dataTransfer.files[0];
  if (file) open(file);
});

// ------------------------------------------------------------------- picking

const raycaster = new THREE.Raycaster();
const pointer = new THREE.Vector2();
let downAt = null;
renderer.domElement.addEventListener("pointerdown", (event) => {
  downAt = [event.clientX, event.clientY];
});
renderer.domElement.addEventListener("pointerup", (event) => {
  // A drag is an orbit, not a click.
  if (!downAt || Math.hypot(event.clientX - downAt[0], event.clientY - downAt[1]) > 4) return;
  if (!current) return;

  pointer.x = (event.clientX / innerWidth) * 2 - 1;
  pointer.y = -(event.clientY / innerHeight) * 2 + 1;
  raycaster.setFromCamera(pointer, camera);
  const hit = raycaster.intersectObjects(current.group.children, false)[0];
  const picked = $("picked");
  if (!hit) {
    picked.className = "none";
    return;
  }
  const expressId = expressIdAt(hit);
  picked.className = "";
  picked.textContent =
    expressId === null
      ? "no id on that triangle"
      : `#${expressId}  ${kernel.getClassName(current.id, expressId) ?? ""}`;
});
