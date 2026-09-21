// SPDX-License-Identifier: Apache-2.0

//! The shared shapes of this package as JSDoc typedefs; the generated
//! declarations name them so a host can type a kernel, a pack or a delta.

/**
 * The kernel class of `@tessifc/core`, from either the web or the Node build.
 * @typedef {import("@tessifc/core").Kernel} Kernel
 */

/**
 * Geometry settings as `evaluateGeometry` and `beginGeometryStream` accept them.
 * @typedef {Record<string, unknown>} GeometrySettings
 */

/**
 * A diagnostic as the kernel reports it.
 * @typedef {object} Diagnostic
 * @property {string} code
 * @property {"info" | "warning" | "error"} severity
 * @property {string} message
 * @property {number | null} [expressId]
 */

/**
 * The JSON index of an IGP pack or chunk.
 * @typedef {object} PackIndex
 * @property {number} igp
 * @property {string} [generator]
 * @property {string} schema
 * @property {{ length_scale_to_m: number }} [units]
 * @property {number[]} model_offset
 * @property {Record<string, unknown> | null} [georef]
 * @property {Array<Record<string, any>>} geometries
 * @property {Record<string, any>} instances
 * @property {string[]} classes
 * @property {Array<Record<string, unknown>>} [provenance]
 * @property {PackMaterial[]} [materials]
 * @property {PackTexture[]} [textures]
 * @property {{ chunk: number, final: boolean, products_done: number, products_total: number }} [stream]
 * @property {Diagnostic[]} diagnostics
 * @property {Record<string, number>} stats
 */

/**
 * One mesh of a pack: positions as x, y, z triples and triangle indices.
 * @typedef {object} PackGeometry
 * @property {number} id
 * @property {string} primitive
 * @property {number[]} bbox
 * @property {boolean | null} closed
 * @property {Float32Array | Float64Array} positions
 * @property {Uint16Array | Uint32Array} indices
 * @property {Float32Array | null} uv Two floats per vertex, or null when the mesh carries none.
 * @property {{ of: number, level: number } | null} lod Set on a coarse level: the base geometry it simplifies, whose positions it shares.
 */

/**
 * One entry of a pack's optional `materials` table.
 * @typedef {object} PackMaterial
 * @property {number[]} color RGBA 0..255, the instance colour.
 * @property {number[] | null} diffuse
 * @property {number[] | null} specular
 * @property {number | null} shininess
 * @property {number | null} roughness
 * @property {string | null} reflectance
 * @property {number | null} texture An id in `textures`.
 * @property {number} source The express id of the surface style.
 */

/**
 * One entry of a pack's optional `textures` table, with its bytes viewed
 * when the pack embeds them.
 * @typedef {object} PackTexture
 * @property {number} id
 * @property {string | null} mime
 * @property {boolean[]} repeat
 * @property {number[] | null} transform A 2D affine `[a, b, c, d, tx, ty]`.
 * @property {string} [uri]
 * @property {Uint8Array} [blob] The encoded image.
 * @property {{ width: number, height: number, components: number, bytes: Uint8Array }} [pixels]
 * @property {boolean} [omitted]
 */

/**
 * The columnar instance table of a pack: one row per placed geometry.
 * @typedef {object} PackInstances
 * @property {number} count
 * @property {Uint32Array} geometryIds
 * @property {Uint32Array} expressIds
 * @property {Uint16Array} classIds
 * @property {Float32Array} transforms
 * @property {Uint8Array} colors
 * @property {Uint16Array} flags
 * @property {Uint32Array | null} provenance
 * @property {Uint32Array | null} material The `materials` index per record, `0xffffffff` for none.
 */

/**
 * A parsed IGP pack, the result of `readIgp`.
 * @typedef {object} Pack
 * @property {PackIndex} index
 * @property {PackGeometry[]} geometry
 * @property {PackInstances} instances
 * @property {number} flags
 * @property {{ chunk: number, final: boolean, products_done: number, products_total: number }} stream
 * @property {number} bytes
 * @property {{ geometryBytes: number, instanceBytes: number, gpuBytes: number }} memory
 */

/**
 * What a script targets: express ids or GlobalIds; the first found is `selected`.
 * @typedef {object} Selection
 * @property {number[]} [ids]
 * @property {string[]} [guids]
 */

/**
 * A script's own result; `changed` and `operations` come from the engine's journal.
 * @typedef {object} ScriptReport
 * @property {boolean} ok
 * @property {string} stdout
 * @property {boolean} changed
 * @property {{ created: number, modified: number, deleted: number }} operations
 * @property {string} [error]
 * @property {string} [traceback]
 * @property {boolean} [timedOut]
 * @property {boolean} [aborted]
 * @property {boolean} [loaded]
 * @property {number} [elapsedMs]
 * @property {string} [label]
 */

/**
 * The kernel's report on a prepared revision.
 * @typedef {object} RevisionImpact
 * @property {boolean} evaluationAccepted
 * @property {boolean} fullRebuild
 * @property {number[]} affectedProducts
 * @property {number[]} removedProducts
 * @property {number[]} [metadataProducts]
 * @property {Array<{ expressId: number, reason: string }>} [reasons]
 * @property {Diagnostic[]} [diagnostics]
 * @property {Array<Record<string, unknown>>} [productOutcomes]
 * @property {number[]} [refusedBooleanProducts]
 * @property {Record<string, number> | null} [timings]
 */

/**
 * A scene delta: what a renderer replaces, drops or refreshes after a change.
 * @typedef {object} Delta
 * @property {"selective" | "full" | "direct"} kind
 * @property {string} revision
 * @property {string} baseRevision
 * @property {Uint8Array} chunk
 * @property {Pack} pack
 * @property {RevisionImpact | null} impact
 * @property {number[]} affectedProducts
 * @property {number[]} removedProducts
 * @property {number[]} metadataProducts
 * @property {boolean} fullRebuild
 * @property {{ nodes: Array<Record<string, unknown>> } | null} hierarchy
 * @property {number[]} [emptyProducts]
 * @property {string} [label]
 * @property {Record<string, unknown>} timings
 */

/**
 * A script runner as `runScriptWith` uses it: `createScriptRunner` in Node, or a host's own.
 * @typedef {object} ScriptRunner
 * @property {(bytes: Uint8Array, source: string, selection?: Selection | null, options?: { commit?: boolean, signal?: AbortSignal | null }) => Promise<ScriptReport & { snapshot: Uint8Array | null }>} run
 * @property {() => Promise<void>} dispose
 * @property {number} timeoutMs
 * @property {boolean} warm
 */

/**
 * An error thrown by a session after the kernel committed; the delta was not completed.
 * @typedef {Error & { committed?: boolean, revision?: string | null, impact?: RevisionImpact }} SessionError
 */

export {};
