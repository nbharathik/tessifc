// SPDX-License-Identifier: Apache-2.0

//! Display formatting shared by every panel.

const numbers = new Intl.NumberFormat();

/** Group digits for a count shown in the interface. */
export function count(value) {
  return numbers.format(Number.isFinite(value) ? value : 0);
}

/** Milliseconds as ms below a second and as seconds above it. */
export function duration(milliseconds) {
  if (!Number.isFinite(milliseconds)) return "-";
  if (milliseconds < 1) return `${milliseconds.toFixed(2)} ms`;
  if (milliseconds < 1000) return `${milliseconds.toFixed(1)} ms`;
  return `${(milliseconds / 1000).toFixed(2)} s`;
}

/** Byte counts in B, kB or MB. */
export function bytes(value) {
  if (!Number.isFinite(value)) return "-";
  if (value < 1024) return `${value} B`;
  if (value < 1024 ** 2) return `${(value / 1024).toFixed(1)} kB`;
  return `${(value / 1024 ** 2).toFixed(1)} MB`;
}

/** One model coordinate, with more decimals close to the origin. */
export function coordinate(value) {
  const number = Number(value);
  if (!Number.isFinite(number)) return "-";
  return number.toFixed(Math.abs(number) < 100 ? 3 : 2);
}

/** A measured length in millimetres or metres. */
export function distance(value) {
  if (!Number.isFinite(value)) return "-";
  if (value < 0.01) return `${(value * 1000).toFixed(1)} mm`;
  return `${value.toFixed(3)} m`;
}

/** An x, y, z point in model units. */
export function point(values) {
  return `${values.map(coordinate).join(" / ")} m`;
}

/** Uppercase the first letter and leave the rest alone. */
export function capitalize(value) {
  return String(value).charAt(0).toUpperCase() + String(value).slice(1);
}

/** The message of an error, whatever kind of value was thrown. */
export function errorText(error) {
  return error instanceof Error ? error.message : String(error);
}

/** Plural helper that keeps call sites short. */
export function plural(value, one, many = `${one}s`) {
  return `${count(value)} ${value === 1 ? one : many}`;
}
