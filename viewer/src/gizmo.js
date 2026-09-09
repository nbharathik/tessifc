// SPDX-License-Identifier: Apache-2.0

//! The orientation gizmo in the corner of the viewport: six axis handles that
//! follow the camera and snap it to an axis when clicked.

const R = 34;
const HANDLE = 9.5;

// IFC is Z up, with Y north and X east.
const AXES = [
  { key: "x+", axis: [1, 0, 0], label: "X", colour: "--axis-x", solid: true },
  { key: "y+", axis: [0, 1, 0], label: "Y", colour: "--axis-y", solid: true },
  { key: "z+", axis: [0, 0, 1], label: "Z", colour: "--axis-z", solid: true },
  { key: "x-", axis: [-1, 0, 0], label: "", colour: "--axis-x", solid: false },
  { key: "y-", axis: [0, -1, 0], label: "", colour: "--axis-y", solid: false },
  { key: "z-", axis: [0, 0, -1], label: "", colour: "--axis-z", solid: false },
];

const NS = "http://www.w3.org/2000/svg";

/**
 * Mount the gizmo into `host` and return its update handle. `onPick` receives
 * the direction the camera should look from.
 */
export function createGizmo({ host, onPick }) {
  const svg = document.createElementNS(NS, "svg");
  svg.setAttribute("viewBox", `${-R - HANDLE} ${-R - HANDLE} ${(R + HANDLE) * 2} ${(R + HANDLE) * 2}`);
  svg.setAttribute("class", "gizmo");
  svg.setAttribute("role", "group");
  svg.setAttribute("aria-label", "Orientation. Click an axis to look along it.");

  const spokes = new Map();
  const handles = new Map();
  for (const spec of AXES) {
    const line = document.createElementNS(NS, "line");
    line.setAttribute("class", "spoke");
    line.setAttribute("x1", "0");
    line.setAttribute("y1", "0");
    line.setAttribute("stroke", `var(${spec.colour})`);
    spokes.set(spec.key, line);

    const group = document.createElementNS(NS, "g");
    group.setAttribute("class", `handle ${spec.solid ? "solid" : "hollow"}`);
    group.setAttribute("tabindex", "0");
    group.setAttribute("role", "button");
    group.setAttribute("aria-label", `Look along ${spec.key.replace("+", " positive").replace("-", " negative")}`);
    const disc = document.createElementNS(NS, "circle");
    disc.setAttribute("r", String(HANDLE));
    disc.setAttribute("fill", spec.solid ? `var(${spec.colour})` : "var(--gizmo-hollow)");
    disc.setAttribute("stroke", `var(${spec.colour})`);
    group.append(disc);
    if (spec.label) {
      const text = document.createElementNS(NS, "text");
      text.setAttribute("text-anchor", "middle");
      text.setAttribute("dy", "0.34em");
      text.textContent = spec.label;
      group.append(text);
    }
    const pick = () => onPick(spec.axis);
    group.addEventListener("pointerdown", (event) => {
      event.preventDefault();
      event.stopPropagation();
      pick();
    });
    group.addEventListener("keydown", (event) => {
      if (event.key !== "Enter" && event.key !== " ") return;
      event.preventDefault();
      pick();
    });
    handles.set(spec.key, group);
  }

  // Spokes first so every handle draws over every line.
  for (const line of spokes.values()) svg.append(line);
  const layer = document.createElementNS(NS, "g");
  svg.append(layer);
  host.append(svg);
  let order = "";

  /** Redraw from the camera basis: right and up span the gizmo's screen plane. */
  function update(basis) {
    if (!basis) return;
    const { right, up, forward } = basis;
    const placed = AXES.map((spec) => {
      const a = spec.axis;
      const x = (a[0] * right[0] + a[1] * right[1] + a[2] * right[2]) * R;
      const y = -(a[0] * up[0] + a[1] * up[1] + a[2] * up[2]) * R;
      const depth = a[0] * forward[0] + a[1] * forward[1] + a[2] * forward[2];
      return { spec, x, y, depth };
    });
    for (const { spec, x, y, depth } of placed) {
      const line = spokes.get(spec.key);
      line.setAttribute("x2", x.toFixed(2));
      line.setAttribute("y2", y.toFixed(2));
      line.setAttribute("opacity", spec.solid ? (depth > 0 ? "0.35" : "0.9") : "0");
      const group = handles.get(spec.key);
      group.setAttribute("transform", `translate(${x.toFixed(2)} ${y.toFixed(2)})`);
      // A handle pointing away from the viewer fades but stays clickable.
      group.setAttribute("opacity", depth > 0 ? "0.45" : "1");
    }
    // Painter's order: the furthest handle is drawn first.
    placed.sort((left, right_) => right_.depth - left.depth);
    const nextOrder = placed.map(({ spec }) => spec.key).join(",");
    if (nextOrder !== order) {
      layer.replaceChildren(...placed.map(({ spec }) => handles.get(spec.key)));
      order = nextOrder;
    }
  }

  return { update, element: svg };
}
