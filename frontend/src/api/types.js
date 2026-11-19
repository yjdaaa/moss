/**
 * MOSS 前端与后端 HTTP / Tauri 契约的 JSDoc 共享类型定义（与 `crates/laser-contracts` 及 `docs/developer/api-contracts.md` 对齐）。
 * 零运行时开销；供 `api/client.js`、`voiceApi.js`、页面与编辑器类型提示/校验共用。
 */

/**
 * @typedef {Object} ApiEnvelope
 * @property {boolean} success
 * @property {*} result
 * @property {Record<string, *>=} detail
 * @property {string=} error_code
 */

/**
 * @typedef {Object} RequestOptions
 * @property {AbortSignal=} signal
 * @property {number=} timeoutMs
 */

/**
 * @typedef {Object} JsonResult
 * @property {number} status
 * @property {boolean} ok
 * @property {boolean=} timedOut
 * @property {ApiEnvelope} data
 */

/**
 * @typedef {Object} PreviewTextResult
 * @property {boolean} ok
 * @property {number} status
 * @property {string} text
 * @property {string} error
 * @property {boolean=} timedOut
 */

/**
 * @typedef {Object} ConnectionSpecInput
 * @property {"serial" | "network"=} connection_mode
 * @property {"serial" | "network"=} preferred_mode
 * @property {string=} port
 * @property {number=} baudrate
 * @property {string=} network_host
 * @property {string=} host
 * @property {number=} network_telnet_port
 * @property {number=} telnet_port
 * @property {number=} network_http_port
 * @property {number=} http_port
 */

/**
 * @typedef {Object} MaterialRecommendation
 * @property {string} material
 * @property {number} thickness_mm
 * @property {number} matched_thickness_mm
 * @property {"surface" | "exact" | "nearest_thickness"} match
 * @property {string} laser_mode
 * @property {number} laser_max_power
 * @property {number} feed_rate
 * @property {number} passes
 * @property {string=} title
 * @property {string=} engraving_mode
 * @property {Record<string, *>=} fill_options
 * @property {Record<string, *>=} cut_options
 */

/**
 * @typedef {Object} SystemInfoResult
 * @property {string} version
 * @property {boolean} auto_check_update
 * @property {boolean} check_prerelease
 * @property {number} actual_port
 * @property {number} preferred_port
 * @property {boolean} is_fallback
 * @property {string} web_host
 * @property {Record<string, *>} mcp
 * @property {{ mode: "official" | "builtin", device_sessions: number | null, phase: string | null, level: number }} voice
 */

/**
 * @typedef {Object} ApprovalItem
 * @property {string} approval_id
 * @property {string} kind
 * @property {string} status
 * @property {string} source
 * @property {string} summary
 * @property {string} target_record
 * @property {string} digest
 * @property {number} created_at
 * @property {number} expires_at
 */

export {};
