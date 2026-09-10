// SPDX-License-Identifier: Apache-2.0

/** Build an immutable hierarchy over indexed bounds; null bounds omit an item. */
export function buildBoundsTree(count, getBounds, leafSize = 8) {
  count = Math.max(0, Math.floor(count));
  leafSize = Math.max(1, Math.floor(leafSize) || 8);
  const boxes = new Float64Array(count * 6);
  const indices = new Uint32Array(count);
  const uncertain = [];
  let valid = 0;
  for (let index = 0; index < count; index += 1) {
    const box = getBounds(index);
    if (!box) continue;
    let finite = true;
    for (let axis = 0; axis < 3; axis += 1) {
      const low = box.min[axis], high = box.max[axis];
      boxes[index * 6 + axis] = low;
      boxes[index * 6 + axis + 3] = high;
      finite &&= Number.isFinite(low) && Number.isFinite(high) && low <= high;
    }
    if (finite) indices[valid++] = index;
    else uncertain.push(index);
  }
  const capacity = valid ? 2 ** Math.ceil(Math.log2(Math.max(1, valid / leafSize))) * 2 - 1 : 0;
  const bounds = new Float64Array(capacity * 6);
  const links = new Int32Array(capacity * 2);
  let used = 0;
  const build = (first, end) => {
    const node = used++;
    const at = node * 6;
    for (let axis = 0; axis < 3; axis += 1) {
      bounds[at + axis] = Infinity;
      bounds[at + axis + 3] = -Infinity;
    }
    for (let cursor = first; cursor < end; cursor += 1) {
      const source = indices[cursor] * 6;
      for (let axis = 0; axis < 3; axis += 1) {
        bounds[at + axis] = Math.min(bounds[at + axis], boxes[source + axis]);
        bounds[at + axis + 3] = Math.max(bounds[at + axis + 3], boxes[source + axis + 3]);
      }
    }
    if (end - first <= leafSize) {
      links[node * 2] = first;
      links[node * 2 + 1] = end - first;
    } else {
      let axis = 0;
      for (let candidate = 1; candidate < 3; candidate += 1) {
        if (bounds[at + candidate + 3] - bounds[at + candidate] > bounds[at + axis + 3] - bounds[at + axis]) axis = candidate;
      }
      const middle = (first + end) >>> 1;
      partition(indices, boxes, first, end - 1, middle, axis);
      links[node * 2] = -build(first, middle);
      links[node * 2 + 1] = build(middle, end);
    }
    return node;
  };
  if (valid) build(0, valid);
  return {
    count,
    bounds: bounds.subarray(0, used * 6),
    links: links.subarray(0, used * 2),
    indices: indices.subarray(0, valid),
    uncertain: Uint32Array.from(uncertain),
  };
}

/** Return possible ray hits in source order; callers retain their exact hit tests. */
export function queryBoundsTree(tree, origin, direction, output = []) {
  output.length = 0;
  const stack = tree.bounds.length ? [0] : [];
  while (stack.length) {
    const node = stack.pop();
    if (!intersects(tree.bounds, node * 6, origin, direction)) continue;
    const first = tree.links[node * 2], second = tree.links[node * 2 + 1];
    if (first < 0) {
      stack.push(-first, second);
    } else {
      for (let cursor = first; cursor < first + second; cursor += 1) output.push(tree.indices[cursor]);
    }
  }
  for (const index of tree.uncertain) output.push(index);
  output.sort((left, right) => left - right);
  return output;
}

function partition(indices, boxes, first, last, middle, axis) {
  const center = (index) => boxes[index * 6 + axis] / 2 + boxes[index * 6 + axis + 3] / 2;
  while (first < last) {
    const pivot = indices[(first + last) >>> 1], value = center(pivot);
    let left = first, right = last;
    while (left <= right) {
      while (center(indices[left]) < value || center(indices[left]) === value && indices[left] < pivot) left++;
      while (center(indices[right]) > value || center(indices[right]) === value && indices[right] > pivot) right--;
      if (left <= right) {
        const temporary = indices[left];
        indices[left++] = indices[right];
        indices[right--] = temporary;
      }
    }
    if (middle <= right) last = right;
    else if (middle >= left) first = left;
    else return;
  }
}

function intersects(bounds, at, origin, direction) {
  let near = 0, far = Infinity;
  for (let axis = 0; axis < 3; axis += 1) {
    if (Math.abs(direction[axis]) < 1e-12) {
      if (origin[axis] < bounds[at + axis] || origin[axis] > bounds[at + axis + 3]) return false;
    } else {
      const inverse = 1 / direction[axis];
      const first = (bounds[at + axis] - origin[axis]) * inverse;
      const second = (bounds[at + axis + 3] - origin[axis]) * inverse;
      near = Math.max(near, Math.min(first, second));
      far = Math.min(far, Math.max(first, second));
      if (near > far) return false;
    }
  }
  return far >= 0;
}
