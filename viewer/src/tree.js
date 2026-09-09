// SPDX-License-Identifier: Apache-2.0

//! The two model trees in the left panel: the IFC spatial structure and the
//! element types. Both descend to individual elements. Nodes are built as data
//! first and become DOM the first time their branch opens, so a large model
//! only pays for what the user looks at.

import { classLabelColor, humanizeIfcClass } from "./igp.js";
import { count as formatCount } from "./format.js";

const TWIST =
  '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" ' +
  'stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m9 6 6 6-6 6" /></svg>';

const EYE_ON =
  '<svg class="eye-on" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" ' +
  'stroke-linejoin="round" aria-hidden="true"><path d="M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12" />' +
  '<circle cx="12" cy="12" r="3" /></svg>';

const EYE_OFF =
  '<svg class="eye-off" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" ' +
  'stroke-linejoin="round" aria-hidden="true"><path d="M4 4l16 16" />' +
  '<path d="M9.9 5.8A9 9 0 0 1 12 5.5c6 0 9.5 6.5 9.5 6.5a16 16 0 0 1-3.3 4M6.6 7.9A16 16 0 0 0 2.5 12S6 18.5 12 18.5a9 9 0 0 0 3.2-.6" /></svg>';

// Past this depth containers fold into their parent instead of recursing, so
// a hostile spatial chain cannot exhaust the stack.
const MAX_DEPTH = 64;

// Elements appear a page at a time, so one class with 50,000 members does not
// build 50,000 rows the moment its group opens.
const PAGE = 300;

// Past this many rows a class group is not paged out just to reveal one
// selected element; the group still opens.
const REVEAL_LIMIT = PAGE * 10;

// Expanding every branch builds rows in slices of this many milliseconds per
// frame, so the page keeps painting while a large tree fills in.
const EXPAND_SLICE_MS = 5;

// One collator for every sort: building one per comparison costs seconds on a large class.
const COLLATOR = new Intl.Collator(undefined, { numeric: true });

const ICON = (path) =>
  `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" ` +
  `stroke-linejoin="round" aria-hidden="true">${path}</svg>`;

const CONTAINER_ICONS = {
  project: ICON('<path d="M3 21h18M6 21V8l6-4 6 4v13" /><path d="M10 21v-5h4v5" />'),
  site: ICON('<path d="M3 18h18" /><path d="m5 18 5-8 4 5 2-3 3 6" /><circle cx="17" cy="6" r="2" />'),
  building: ICON('<rect x="5" y="3" width="14" height="18" rx="1" /><path d="M9 7h2m3 0h2M9 11h2m3 0h2M9 15h2m3 0h2" />'),
  storey: ICON('<path d="m3 8 9-5 9 5-9 5z" /><path d="m3 13 9 5 9-5" />'),
  space: ICON('<path d="M4 20V7l8-4 8 4v13z" /><path d="M4 12h16" />'),
  group: ICON('<path d="M3 7h6l2 2h10v10H3z" />'),
};

const ROLE_ORDER = [
  "Spatial structure",
  "Structure",
  "Envelope and openings",
  "Building services",
  "Equipment and objects",
  "Other elements",
];

/** Parse one markup fragment once; rows clone it instead of parsing again. */
function fragment(markup) {
  const template = document.createElement("template");
  template.innerHTML = markup;
  return template.content.firstElementChild;
}

/** A blank row of one kind. Cloning it is several times cheaper than building it. */
function rowTemplate(branch) {
  const wrapper = document.createElement("div");
  wrapper.setAttribute("role", "treeitem");
  wrapper.tabIndex = -1;
  const row = document.createElement("div");
  row.className = "trow";
  const twist = document.createElement("button");
  twist.type = "button";
  twist.className = "twist";
  twist.tabIndex = -1;
  twist.setAttribute("aria-hidden", "true");
  if (branch) twist.append(fragment(TWIST));
  const mark = document.createElement("span");
  mark.setAttribute("aria-hidden", "true");
  mark.className = "swatch";
  const name = document.createElement("span");
  name.className = "name";
  const kind = document.createElement("span");
  kind.className = "kind";
  const tail = document.createElement("span");
  tail.className = branch ? "count" : "eid";
  const eye = document.createElement("button");
  eye.type = "button";
  eye.className = "eye";
  eye.tabIndex = -1;
  eye.append(fragment(EYE_ON), fragment(EYE_OFF));
  row.append(twist, mark, name, kind, tail, eye);
  wrapper.append(row);
  if (branch) {
    wrapper.setAttribute("aria-expanded", "false");
    const body = document.createElement("div");
    body.className = "tkids";
    body.setAttribute("role", "group");
    wrapper.append(body);
  }
  return wrapper;
}

let templates = null;
let containerIconNodes = null;

export function createTree({ onVisibility, onSelect, onFocus }) {
  const spatialHost = document.getElementById("tree-spatial");
  const typesHost = document.getElementById("tree-types");
  const searchInput = document.getElementById("tree-search");
  templates ??= { branch: rowTemplate(true), element: rowTemplate(false) };
  containerIconNodes ??= Object.fromEntries(
    Object.entries(CONTAINER_ICONS).map(([key, markup]) => [key, fragment(markup)]),
  );

  let roots = { spatial: [], types: [] };
  let spatialItems = new Map();
  let mounted = new Set();
  let branches = [];
  let query = "";
  let searchFrame = 0;
  let selectedId = null;
  let selectedNodes = [];
  let expansion = null;
  // The current visibility rule, so a row built later starts in the right state.
  let isVisible = () => true;
  const nodeByElement = new WeakMap();
  const tabStops = new Map();

  searchInput.addEventListener("input", () => {
    if (searchFrame) return;
    searchFrame = requestAnimationFrame(() => {
      searchFrame = 0;
      applySearch(searchInput.value);
    });
  });

  // One listener per tree instead of five per row: a row is found from the event target.
  for (const host of [spatialHost, typesHost]) {
    host.setAttribute("role", "tree");
    host.addEventListener("keydown", (event) => onKeyDown(host, event));
    host.addEventListener("click", (event) => onClick(host, event));
    host.addEventListener("dblclick", (event) => {
      const node = nodeAt(host, event.target);
      if (node && !event.target.closest(".twist, .eye")) onFocus(collectRecords(node));
    });
    host.addEventListener("focusin", (event) => {
      const item = event.target.closest?.(".tnode");
      if (item && host.contains(item)) setTabStop(host, item);
    });
  }

  function onClick(host, event) {
    const more = event.target.closest?.(".tmore");
    if (more && host.contains(more)) {
      const node = nodeByElement.get(more.parentElement?.parentElement);
      if (node?.isClass) appendPage(node, more.parentElement);
      return;
    }
    const node = nodeAt(host, event.target);
    if (!node) return;
    if (event.target.closest(".twist")) {
      if (node.kind === "branch") setOpen(node, !node.open);
      return;
    }
    if (event.target.closest(".eye")) {
      toggleVisibility(node);
      return;
    }
    activate(node);
  }

  /** The node whose row contains `target`, or null for anything between rows. */
  function nodeAt(host, target) {
    const row = target.closest?.(".trow");
    if (!row || !host.contains(row)) return null;
    return nodeByElement.get(row.parentElement) ?? null;
  }

  /** Drop every node and show the empty state again. */
  function clear() {
    cancelExpansion();
    roots = { spatial: [], types: [] };
    spatialItems = new Map();
    mounted = new Set();
    branches = [];
    query = "";
    selectedId = null;
    selectedNodes = [];
    tabStops.clear();
    isVisible = () => true;
    searchInput.value = "";
    searchInput.disabled = true;
    spatialHost.replaceChildren(emptyState("No model open", "Open an IFC to browse its project, site, building and storeys."));
    typesHost.replaceChildren(emptyState("No element types", "Type groups appear once a model is converted."));
  }

  /** Build both trees for one model from the per-product record lists in `index`. */
  function build(pack, hierarchy, index) {
    cancelExpansion();
    mounted = new Set();
    branches = [];
    query = "";
    selectedId = null;
    selectedNodes = [];
    tabStops.clear();
    isVisible = () => true;
    searchInput.value = "";
    searchInput.disabled = false;

    const products = productTable(pack, hierarchy, index);
    roots.types = buildTypeRoots(index, products);
    roots.spatial = buildSpatialRoots(hierarchy, index, products);
    // Without a spatial hierarchy the type roots stand in, built again so the
    // two trees own separate nodes.
    if (!roots.spatial.length) roots.spatial = buildTypeRoots(index, products);

    mountRoots(spatialHost, roots.spatial);
    mountRoots(typesHost, roots.types);
  }

  /** One entry per rendered product: its name, class, records and container. */
  function productTable(pack, hierarchy, index) {
    const named = new Map();
    for (const node of Array.isArray(hierarchy?.nodes) ? hierarchy.nodes : []) {
      if (Number.isInteger(node?.expressId)) named.set(node.expressId, node);
    }
    const table = new Map();
    for (const [expressId, records] of index.recordsByExpressId) {
      if (!records.length) continue;
      const known = named.get(expressId);
      const exact = known?.class ?? String(pack.index.classes[pack.instances.classIds[records[0]]] ?? "IfcProduct");
      table.set(expressId, {
        expressId,
        exact,
        records,
        parentExpressId: known?.parentExpressId ?? null,
        label: clip(known?.name?.trim()) || `${humanizeIfcClass(exact)} #${expressId}`,
      });
      const entry = table.get(expressId);
      entry.key = `${entry.label} ${exact} #${expressId}`.toLowerCase();
    }
    return table;
  }

  // ------------------------------------------------------- type grouping

  function buildTypeRoots(index, products) {
    const byClass = new Map();
    for (const product of products.values()) {
      let list = byClass.get(product.exact);
      if (!list) byClass.set(product.exact, (list = []));
      list.push(product);
    }
    const groups = new Map();
    for (const item of index.classes.values()) {
      const role = buildingRole(item.exact);
      if (!groups.has(role)) groups.set(role, []);
      groups.get(role).push(item);
    }
    return ROLE_ORDER.filter((role) => groups.has(role)).map((role) => {
      const items = groups.get(role).sort((left, right) => left.exact.localeCompare(right.exact));
      const children = items.map((item) =>
        classGroup({
          exact: item.exact,
          products: sortProducts(byClass.get(item.exact) ?? []),
          classId: item.id,
          depth: 1,
          ancestors: [role],
        }),
      );
      return branch({
        label: role,
        exact: "IFC role",
        depth: 0,
        children,
        ancestors: [],
      });
    });
  }

  // ---------------------------------------------------- spatial grouping

  function buildSpatialRoots(hierarchy, index, products) {
    const nodes = Array.isArray(hierarchy?.nodes)
      ? hierarchy.nodes.filter((node) => Number.isInteger(node?.expressId) && typeof node.class === "string")
      : [];
    spatialItems = new Map(nodes.map((node) => [node.expressId, { ...node, children: [] }]));
    if (!nodes.length) return [];

    const items = spatialItems;
    const tops = [];
    for (const item of items.values()) {
      const parent = items.get(item.parentExpressId);
      if (parent && parent !== item) parent.children.push(item);
      else tops.push(item);
    }
    const order = (left, right) =>
      spatialRank(left.class) - spatialRank(right.class) ||
      String(left.name ?? left.class).localeCompare(String(right.name ?? right.class));
    tops.sort(order);
    for (const item of items.values()) item.children.sort(order);

    const visited = new Set();
    const result = [];
    const loose = [];
    for (const top of tops) collect(top, result, loose, visited, products, 0, []);
    for (const item of items.values()) collect(item, result, loose, visited, products, 0, []);
    // Products the file never placed in a container still have to be reachable.
    for (const product of products.values()) {
      if (!visited.has(product.expressId)) {
        visited.add(product.expressId);
        loose.push(product);
      }
    }
    if (loose.length) {
      const node = groupBranch("Unassigned elements", loose, 0, []);
      if (node) result.push(node);
    }
    return result;
  }

  function collect(item, result, loose, visited, products, depth, ancestors) {
    if (visited.has(item.expressId)) return;
    const product = products.get(item.expressId);
    if (product && !item.children.length) {
      visited.add(item.expressId);
      loose.push(product);
      return;
    }
    const node = spatialBranch(item, visited, products, depth, ancestors);
    if (node) result.push(node);
  }

  function spatialBranch(item, visited, products, depth, ancestors) {
    if (visited.has(item.expressId) || depth > MAX_DEPTH) return null;
    visited.add(item.expressId);

    const label = clip(item.name?.trim()) || humanizeIfcClass(item.class);
    const path = [...ancestors, label, item.class];
    const containers = [];
    const leaves = [];
    for (const child of item.children) {
      if (child.children.length) {
        containers.push(child);
        continue;
      }
      visited.add(child.expressId);
      const product = products.get(child.expressId);
      if (product) leaves.push(product);
    }
    // A container that also carries geometry lists itself among its contents.
    const own = products.get(item.expressId);
    if (own) leaves.unshift(own);

    const children = [];
    for (const child of containers) {
      const node = spatialBranch(child, visited, products, depth + 1, path);
      if (node) children.push(node);
    }
    children.push(...classGroups(leaves, depth + 1, path));

    return branch({
      label,
      exact: item.class,
      depth,
      children,
      ancestors: path,
      expressId: item.expressId,
      hasGeometry: Boolean(own),
    });
  }

  function groupBranch(label, products, depth, ancestors) {
    const children = classGroups(products, depth + 1, [...ancestors, label]);
    if (!children.length) return null;
    return branch({ label, exact: "IfcProduct", depth, children, ancestors });
  }

  /** One group per IFC class inside a container, each holding its elements. */
  function classGroups(products, depth, ancestors) {
    const groups = new Map();
    for (const product of products) {
      let list = groups.get(product.exact);
      if (!list) groups.set(product.exact, (list = []));
      list.push(product);
    }
    return [...groups.entries()]
      .sort(([left], [right]) => left.localeCompare(right))
      .map(([exact, list]) => classGroup({ exact, products: sortProducts(list), classId: null, depth, ancestors }));
  }

  // ------------------------------------------------------- node factories

  function branch({ label, exact, depth, children, ancestors, expressId, hasGeometry = false }) {
    const node = {
      kind: "branch",
      label,
      exact,
      depth,
      children,
      expressId: expressId ?? null,
      hasGeometry,
      classId: null,
      total: children.reduce((sum, child) => sum + child.total, 0),
      search: [...ancestors, label, exact, expressId ? `#${expressId}` : ""].join(" ").toLowerCase(),
      parent: null,
      element: null,
      row: null,
      eye: null,
      open: false,
      built: false,
      hidden: false,
      off: false,
      mixed: false,
    };
    for (const child of children) child.parent = node;
    branches.push(node);
    return node;
  }

  /** A class inside one container: a branch whose children are its elements. */
  function classGroup({ exact, products, classId, depth, ancestors }) {
    const node = {
      kind: "branch",
      isClass: true,
      label: humanizeIfcClass(exact),
      exact,
      depth,
      products,
      children: [],
      shown: 0,
      expressId: null,
      classId,
      total: products.length,
      search: [...ancestors, humanizeIfcClass(exact), exact].join(" ").toLowerCase(),
      parent: null,
      element: null,
      row: null,
      eye: null,
      open: false,
      built: false,
      hidden: false,
      off: false,
      mixed: false,
      dormant: false,
      stale: false,
    };
    branches.push(node);
    return node;
  }

  function element(product, depth, parent) {
    return {
      kind: "element",
      label: product.label,
      exact: product.exact,
      depth,
      records: product.records,
      expressId: product.expressId,
      classId: null,
      children: [],
      total: 1,
      search: `${parent.search} ${product.label} #${product.expressId}`.toLowerCase(),
      parent,
      element: null,
      row: null,
      eye: null,
      open: false,
      built: true,
      hidden: false,
      off: !product.records.some(isVisible),
      mixed: false,
    };
  }

  // ------------------------------------------------------------ mounting

  function mountRoots(host, list) {
    if (!list.length) {
      host.replaceChildren(emptyState("Nothing to show", "This model produced no renderable products."));
      return;
    }
    // Built detached and attached once, so the rows never shift each other on the page.
    const roots = list.map(mount);
    // Containers open down to the storeys; the class groups inside stay closed.
    for (const node of list) openContainers(node);
    host.replaceChildren(...roots);
    const first = list[0]?.element;
    if (first) setTabStop(host, first);
  }

  function openContainers(node) {
    if (node.kind !== "branch" || node.isClass) return;
    setOpen(node, true);
    for (const child of node.children) openContainers(child);
  }

  function mount(node) {
    if (node.element) return node.element;
    const wrapper = templates[node.kind === "branch" ? "branch" : "element"].cloneNode(true);
    wrapper.className = `tnode ${node.kind}${node.isClass ? " class-group" : ""}`;
    wrapper.setAttribute("aria-level", String(node.depth + 1));
    const row = wrapper.firstElementChild;
    row.style.setProperty("--indent", `${4 + node.depth * 13}px`);
    const [, mark, name, kind, tail, eye] = row.children;

    if (node.kind === "branch" && !node.isClass) {
      mark.className = "cicon";
      mark.append(containerIconNodes[containerIcon(node.exact)].cloneNode(true));
    } else {
      mark.style.background = classLabelColor(node.exact);
    }
    name.textContent = node.label;
    const described = node.expressId === null ? `${node.label}  ${node.exact}` : `${node.label}  ${node.exact}  #${node.expressId}`;
    row.title = described;
    // A tree item is named from its contents, which here is the whole subtree.
    wrapper.setAttribute("aria-label", described);
    kind.textContent = node.exact;
    tail.textContent = node.kind === "branch" ? formatCount(node.total) : `#${node.expressId}`;

    node.row = row;
    node.eye = eye;
    node.element = wrapper;
    node.shownOff = false;
    node.shownMixed = false;
    node.shownHidden = false;
    nodeByElement.set(wrapper, node);
    if (node.kind !== "branch") mounted.add(node);
    // While the browser skips an off-screen list, its rows keep their state in data
    // only; the rows are brought up to date when the list is on screen again.
    if (node.isClass) {
      wrapper.lastElementChild.addEventListener("contentvisibilityautostatechange", (event) => {
        node.dormant = Boolean(event.skipped);
        if (node.dormant || !node.stale) return;
        node.stale = false;
        for (const child of node.children) syncRow(child);
      });
    }
    applyState(node, true);
    return wrapper;
  }

  function setOpen(node, open) {
    if (node.kind !== "branch") return;
    node.open = open;
    node.expandPending = false;
    if (!node.element) return;
    node.element.classList.toggle("open", open);
    node.element.setAttribute("aria-expanded", String(open));
    if (open) buildChildren(node);
  }

  function buildChildren(node) {
    if (node.built) return;
    node.built = true;
    const body = node.element.lastElementChild;
    if (node.isClass) {
      appendPage(node, body);
      return;
    }
    if (!node.children.length) {
      const empty = document.createElement("p");
      empty.className = "tree-empty";
      empty.textContent = "No rendered elements";
      body.append(empty);
      return;
    }
    body.append(...node.children.map(mount));
    if (query) for (const child of node.children) matchNode(child);
  }

  /** Add the next page of elements to a class group, with a row for the rest. */
  function appendPage(node, body) {
    if (body.lastElementChild?.classList.contains("tmore")) body.lastElementChild.remove();
    const next = node.products.slice(node.shown, node.shown + PAGE);
    const created = next.map((product) => {
      const child = element(product, node.depth + 1, node);
      node.children.push(child);
      return child;
    });
    node.shown += next.length;
    body.append(...created.map(mount));
    if (query) for (const child of created) matchNode(child);
    const rest = node.products.length - node.shown;
    // The list's size while it is off screen and skipped, so the scrollbar does not jump.
    body.style.setProperty("--rows", String(node.shown + (rest > 0 ? 1 : 0)));
    if (rest <= 0) return;
    const more = document.createElement("button");
    more.type = "button";
    more.className = "tmore";
    more.style.setProperty("--indent", `${4 + (node.depth + 1) * 13}px`);
    more.textContent = `Show ${formatCount(Math.min(rest, PAGE))} more of ${formatCount(rest)}`;
    body.append(more);
  }

  // ------------------------------------------------------- interaction

  function activate(node) {
    if (node.kind === "element") {
      onSelect(node.expressId);
      return;
    }
    if (node.expressId !== null && node.hasGeometry) {
      onSelect(node.expressId);
      return;
    }
    setOpen(node, !node.open);
  }

  /** Every record under a node, walking the data tree rather than the DOM. */
  function collectRecords(node) {
    const records = [];
    const stack = [node];
    while (stack.length) {
      const item = stack.pop();
      if (item.kind === "element") {
        records.push(...item.records);
        continue;
      }
      if (item.isClass) {
        for (const product of item.products) records.push(...product.records);
        continue;
      }
      stack.push(...item.children);
    }
    return records;
  }

  function toggleVisibility(node) {
    const visible = node.off;
    if (node.isClass && node.classId !== null) {
      onVisibility({ classId: node.classId, records: null }, visible);
      return;
    }
    onVisibility({ classId: null, records: collectRecords(node) }, visible);
  }

  // -------------------------------------------------------- keyboard

  /** The items a user can step through, in order: mounted, unfiltered and under open branches. Read from the data, so no layout is forced. */
  function visibleRows(host) {
    const rows = [];
    const walk = (node) => {
      if (!node.element || node.hidden) return;
      rows.push(node.element);
      if (node.kind !== "branch" || !node.open) return;
      for (const child of node.children) walk(child);
    };
    for (const node of host === spatialHost ? roots.spatial : roots.types) walk(node);
    return rows;
  }

  /** One row per tree carries the tab stop; only the previous stop and the new one change. */
  function setTabStop(host, item) {
    const previous = tabStops.get(host);
    if (previous === item) return;
    if (previous) previous.tabIndex = -1;
    item.tabIndex = 0;
    tabStops.set(host, item);
  }

  function onKeyDown(host, event) {
    const item = event.target.closest?.(".tnode");
    if (!item) return;
    const node = nodeByElement.get(item);
    if (!node) return;
    const rows = visibleRows(host);
    const index = rows.indexOf(item);
    const move = (next) => {
      const target = rows[next];
      if (!target) return;
      setTabStop(host, target);
      target.focus();
    };
    switch (event.key) {
      case "ArrowDown": move(index + 1); break;
      case "ArrowUp": move(index - 1); break;
      case "Home": move(0); break;
      case "End": move(rows.length - 1); break;
      case "ArrowRight":
        if (node.kind === "branch" && !node.open) setOpen(node, true);
        else move(index + 1);
        break;
      case "ArrowLeft":
        if (node.kind === "branch" && node.open) setOpen(node, false);
        else if (node.parent?.element) { setTabStop(host, node.parent.element); node.parent.element.focus(); }
        break;
      case "Enter": activate(node); break;
      case " ": toggleVisibility(node); break;
      case "f": case "F": onFocus(collectRecords(node)); break;
      default: return;
    }
    event.preventDefault();
    event.stopPropagation();
  }

  // -------------------------------------------------------------- search

  function applySearch(value) {
    query = value.trim().toLowerCase();
    for (const node of roots.spatial) matchNode(node);
    for (const node of roots.types) matchNode(node);
  }

  function matchNode(node) {
    if (node.kind === "element") {
      const visible = !query || node.search.includes(query);
      node.hidden = !visible;
      applyState(node);
      return visible;
    }
    const self = !query || node.search.includes(query);
    if (query && node.isClass && !self) revealMatches(node);
    let childVisible = false;
    for (const child of node.children) childVisible = matchNode(child) || childVisible;
    // Members on pages that are not built yet still count, so the group is not hidden.
    if (query && node.isClass && !childVisible) childVisible = node.products.some((product) => product.key.includes(query));
    if (query && !node.isClass && !childVisible && !self) {
      for (const child of node.children) {
        child.hidden = true;
        applyState(child);
      }
    }
    const visible = self || childVisible;
    node.hidden = !visible;
    applyState(node);
    if (node.element && query && childVisible) setOpen(node, true);
    return visible;
  }

  /** Build the pages of a class group up to its last matching member, within a bound. */
  function revealMatches(node) {
    if (!node.element) return;
    let last = -1;
    const bound = Math.min(node.products.length, node.shown + PAGE * 3);
    for (let index = 0; index < bound; index += 1) {
      if (node.products[index].key.includes(query)) last = index;
    }
    if (last < 0) return;
    setOpen(node, true);
    const body = node.element.lastElementChild;
    while (body && node.shown <= last && node.shown < node.products.length) appendPage(node, body);
  }

  // ------------------------------------------------------------ external

  /** Refresh every mounted row from the current visibility predicate. */
  function syncVisibility(predicate) {
    isVisible = predicate;
    for (const node of mounted) {
      setOffState(node, !node.records.some(isVisible));
    }
    rollUp();
  }

  /** Reset every row to visible without consulting the predicate. */
  function markAllVisible() {
    isVisible = () => true;
    for (const node of mounted) setOffState(node, false);
    for (const node of branches) setOffState(node, false);
  }

  function setOffState(node, off, mixed = false) {
    node.off = off;
    node.mixed = mixed;
    applyState(node);
  }

  /** Reflect a node's stored state in its row, unless the row sits in a list the browser is skipping. */
  function applyState(node, force = false) {
    if (!node.element) return;
    if (!force && node.parent?.dormant) {
      node.parent.stale = true;
      return;
    }
    syncRow(node);
  }

  /** Write a row's on, mixed and filtered state, only what changed. */
  function syncRow(node) {
    const item = node.element;
    if (!item) return;
    const off = Boolean(node.off);
    const mixed = Boolean(node.mixed);
    const hidden = Boolean(node.hidden);
    if (node.shownOff !== off || node.shownMixed !== mixed) {
      node.shownOff = off;
      node.shownMixed = mixed;
      item.classList.toggle("off", off);
      item.classList.toggle("mixed", mixed);
      // The eye draws its open or crossed icon from the row's class; only its name changes here.
      node.eye?.setAttribute("aria-label", `${off ? "Show" : "Hide"} ${node.label}`);
    }
    if (node.shownHidden !== hidden) {
      node.shownHidden = hidden;
      item.classList.toggle("hidden", hidden);
    }
  }

  /** A branch is off when everything under it is, and mixed when only some is. */
  function rollUp() {
    const walk = (node) => {
      if (node.kind === "element") return { off: node.off ? 1 : 0, total: 1 };
      let off = 0;
      let total = 0;
      if (node.isClass) {
        // Members without rows are read from the rule, so a closed group reports its true state.
        for (const product of node.products) {
          total += 1;
          if (!product.records.some(isVisible)) off += 1;
        }
      } else {
        for (const child of node.children) {
          const result = walk(child);
          off += result.off;
          total += result.total;
        }
      }
      const allOff = total > 0 && off === total;
      setOffState(node, allOff, off > 0 && !allOff);
      return { off, total };
    };
    for (const node of roots.spatial) walk(node);
    for (const node of roots.types) walk(node);
  }

  /** The spatial containers above one element, top down, for the element panel. */
  function pathOf(expressId) {
    const chain = [];
    const seen = new Set([expressId]);
    let item = spatialItems.get(expressId);
    while (item && chain.length < MAX_DEPTH) {
      const parent = spatialItems.get(item.parentExpressId);
      if (!parent || seen.has(parent.expressId)) break;
      seen.add(parent.expressId);
      chain.unshift({
        expressId: parent.expressId,
        class: parent.class,
        name: clip(parent.name?.trim()) || humanizeIfcClass(parent.class),
      });
      item = parent;
    }
    return chain;
  }

  /**
   * Mark the selected element in both trees, opening and scrolling to it in the
   * spatial tree so a viewport click always shows where the element lives.
   */
  function select(expressId) {
    selectedId = Number.isInteger(expressId) ? expressId : null;
    for (const node of selectedNodes) {
      node.row?.classList.remove("selected");
      node.element?.removeAttribute("aria-selected");
    }
    selectedNodes = [];
    if (selectedId === null) return;
    const found = reveal(roots.spatial, selectedId) || reveal(roots.types, selectedId);
    if (!found) return;
    markSelected(found);
    found.row?.scrollIntoView({ block: "nearest" });
    for (const node of markInTypes(selectedId)) markSelected(node);
  }

  function markSelected(node) {
    node.row?.classList.add("selected");
    node.element?.setAttribute("aria-selected", "true");
    selectedNodes.push(node);
  }

  /** Open the ancestors of one element and build the page that holds it. */
  function reveal(list, expressId) {
    const stack = [...list];
    while (stack.length) {
      const node = stack.pop();
      if (node.kind === "element") {
        if (node.expressId === expressId) return node;
        continue;
      }
      if (node.isClass) {
        const at = node.products.findIndex((product) => product.expressId === expressId);
        if (at < 0) continue;
        openAncestors(node);
        setOpen(node, true);
        const body = node.element?.lastElementChild;
        while (body && node.shown <= at && node.shown < REVEAL_LIMIT) appendPage(node, body);
        return node.children.find((child) => child.expressId === expressId) ?? null;
      }
      stack.push(...node.children);
    }
    return null;
  }

  function markInTypes(expressId) {
    const found = [];
    const stack = [...roots.types];
    while (stack.length) {
      const node = stack.pop();
      if (node.kind === "element" && node.expressId === expressId) found.push(node);
      else stack.push(...node.children);
    }
    return found;
  }

  function openAncestors(node) {
    const chain = [];
    for (let item = node.parent; item; item = item.parent) chain.unshift(item);
    for (const item of chain) setOpen(item, true);
  }

  /** Open every branch, or collapse back to the spatial containers. */
  function expand(all) {
    cancelExpansion();
    if (!all) {
      const walk = (node) => {
        if (node.kind !== "branch") return;
        setOpen(node, !node.isClass);
        for (const child of node.children) walk(child);
      };
      for (const list of [roots.spatial, roots.types]) for (const node of list) walk(node);
      return;
    }
    // Containers open at once; class groups build their rows in slices, so the
    // page keeps painting while a large model fills in.
    const pending = [];
    const walk = (node) => {
      if (node.kind !== "branch") return;
      if (node.isClass) {
        if (!node.open) {
          node.expandPending = true;
          pending.push(node);
        }
      } else setOpen(node, true);
      for (const child of node.children) walk(child);
    };
    for (const list of [roots.spatial, roots.types]) for (const node of list) walk(node);
    // The first rows arrive a frame after the click, so the click's own frame stays light.
    expansion = { pending, index: 0, frame: 0 };
    expansion.frame = requestAnimationFrame(continueExpansion);
  }

  function continueExpansion() {
    const work = expansion;
    if (!work) return;
    work.frame = 0;
    const deadline = performance.now() + EXPAND_SLICE_MS;
    while (work.index < work.pending.length) {
      const node = work.pending[work.index];
      work.index += 1;
      // A group the user touched meanwhile keeps the state they gave it.
      if (node.expandPending) setOpen(node, true);
      if (performance.now() >= deadline) break;
    }
    if (work.index < work.pending.length) work.frame = requestAnimationFrame(continueExpansion);
    else expansion = null;
  }

  function cancelExpansion() {
    if (!expansion) return;
    if (expansion.frame) cancelAnimationFrame(expansion.frame);
    for (const node of expansion.pending) node.expandPending = false;
    expansion = null;
  }

  return {
    build,
    clear,
    syncVisibility,
    markAllVisible,
    select,
    pathOf,
    expand,
    focusSearch: () => searchInput.focus(),
    expanding: () => Boolean(expansion),
  };
}

/** Bound a file-supplied label so one long string cannot stall layout. */
function clip(value, limit = 120) {
  const text = String(value ?? "");
  return text.length > limit ? `${text.slice(0, limit - 1)}...` : text;
}

/** Elements read in name order, with a numeric tail sorted as a number. */
function sortProducts(list) {
  return list.sort((left, right) => COLLATOR.compare(left.label, right.label) || left.expressId - right.expressId);
}

function emptyState(title, sub) {
  const wrapper = document.createElement("div");
  wrapper.className = "empty";
  wrapper.innerHTML =
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" ' +
    'stroke-linejoin="round" aria-hidden="true"><path d="M4 20V9l8-5 8 5v11z" /><path d="M9 20v-6h6v6" /></svg>';
  const strong = document.createElement("strong");
  strong.textContent = title;
  const note = document.createElement("span");
  note.className = "sub";
  note.textContent = sub;
  wrapper.append(strong, note);
  return wrapper;
}

/** The icon key for a spatial container, by IFC class. */
function containerIcon(className) {
  const value = String(className).toLowerCase();
  if (value.includes("project")) return "project";
  if (value.includes("site")) return "site";
  if (value.includes("buildingstorey")) return "storey";
  if (value.includes("building")) return "building";
  if (value.includes("space") || value.includes("zone")) return "space";
  return "group";
}

/** Sort order for the spatial containers of a project. */
function spatialRank(className) {
  const value = String(className).toLowerCase();
  if (value.includes("project")) return 0;
  if (value.includes("site")) return 1;
  if (value.includes("buildingstorey")) return 3;
  if (value.includes("building")) return 2;
  if (value.includes("space")) return 4;
  return 5;
}

/** Group an IFC class under a readable role for the type tree. */
function buildingRole(className) {
  const value = String(className).toLowerCase();
  if (/(project|site|building|storey|space|zone)/.test(value)) return "Spatial structure";
  if (/(wall|slab|roof|beam|column|footing|member|plate|stair|ramp|railing)/.test(value)) return "Structure";
  if (/(door|window|opening|covering|curtain|shading)/.test(value)) return "Envelope and openings";
  if (/(flow|distribution|pipe|duct|cable|terminal|sanitary|system|port)/.test(value)) return "Building services";
  if (/(furnishing|equipment|proxy|transport)/.test(value)) return "Equipment and objects";
  return "Other elements";
}
