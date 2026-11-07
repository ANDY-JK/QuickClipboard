// 扩展 Worker 沙箱运行时
//
// 使用 Blob + `new Worker(url, { type: 'module' })` 加载扩展入口源码。
// host 与扩展之间只通过 postMessage 通信，扩展无法直接触碰宿主 DOM 或 Tauri API。

import { invoke } from '@tauri-apps/api/core'
import { ERROR_CATEGORIES, LIMITS, MESSAGE_TYPES } from '../protocol'

const ITEM_KEYS = new Set(['kind', 'label', 'detail'])

// 前置注入：为扩展提供最小 host API
const WORKER_PREFIX = `
self.qc = {
  log: (...args) => {
    const safe = args.map((value) => {
      try {
        return typeof value === 'string' ? value : JSON.stringify(value)
      } catch (_) {
        return '[unserializable]'
      }
    })
    self.postMessage({ type: 'log', args: safe })
  }
};
`

// 后置注入：加载入口模块并处理识别请求。
// 入口既支持命名导出 `extension`，也支持 `export default`；
// 两者都没有时才判失败。
const WORKER_SUFFIX = `
const QC_EXTENSION_URL = '__QC_EXTENSION_URL__';

let __qcExtension = null;
let __qcLoadError = null;

// 用动态 import 而不是直接读取模块内绑定：匿名默认导出（export default {...}）
// 在模块内部没有可引用的局部名字，只能通过模块命名空间拿到 default。
const __qcReady = import(QC_EXTENSION_URL).then(
  (mod) => {
    const named = mod && mod.extension;
    const fallback = mod && mod.default;
    const candidate = named && typeof named === 'object' ? named : fallback;
    if (candidate && typeof candidate === 'object') {
      __qcExtension = candidate;
    } else {
      __qcLoadError = '扩展未导出 extension 对象（命名导出或默认导出其一）';
    }
  },
  (error) => {
    __qcLoadError = '扩展入口加载失败: ' + String((error && error.message) || error);
  }
);

self.addEventListener('message', async (event) => {
  const message = event.data || {};
  if (message.type !== 'identify' && message.type !== 'probe') return;
  const requestId = message.requestId;
  // category 是宿主侧判定「条目级 / 运行级」的唯一依据：
  // 入口加载失败属于运行级，单条 identify 抛错属于条目级。
  const reply = (items, error, category) => {
    try {
      self.postMessage({ type: 'result', requestId, items, error, category });
    } catch (replyError) {
      self.postMessage({
        type: 'result',
        requestId,
        items: [],
        error: String((replyError && replyError.message) || replyError),
        category: 'RUNTIME_ERROR',
      });
    }
  };

  if (!__qcExtension || typeof __qcExtension.identify !== 'function') {
    await __qcReady;
  }

  if (!__qcExtension || typeof __qcExtension.identify !== 'function') {
    reply([], __qcLoadError || '扩展未导出 extension.identify 函数', 'RUNTIME_ERROR');
    return;
  }

  // probe 只验证入口可加载且导出约定成立，不执行业务识别，
  // 供设置页在扩展真正被使用前就暴露加载失败。
  if (message.type === 'probe') {
    reply([]);
    return;
  }

  try {
    const result = await __qcExtension.identify(message.context || {});
    if (Array.isArray(result)) {
      reply(result);
    } else {
      reply([], 'identify 必须返回数组', 'ITEM_ERROR');
    }
  } catch (error) {
    reply([], String((error && error.message) || error), 'ITEM_ERROR');
  }
});
`

function normalizeItem(raw) {
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) {
    return null
  }

  for (const key of Object.keys(raw)) {
    if (!ITEM_KEYS.has(key)) {
      return null
    }
  }

  if (typeof raw.kind !== 'string' || raw.label === undefined) {
    return null
  }

  const kind = raw.kind.trim()
  const label = typeof raw.label === 'string' ? raw.label.trim() : ''
  const detail = raw.detail === undefined || raw.detail === null ? '' : raw.detail

  if (!kind || kind.length > LIMITS.maxKindLength) return null
  if (!label || label.length > LIMITS.maxLabelLength) return null
  if (typeof detail !== 'string' || detail.length > LIMITS.maxDetailLength) return null

  return { kind, label, detail }
}

function validateBatch(rawItems) {
  if (!Array.isArray(rawItems)) {
    return { valid: false, items: [], invalid: 0, truncated: 0 }
  }

  const accepted = []
  let invalid = 0
  for (const raw of rawItems.slice(0, LIMITS.maxItems)) {
    const item = normalizeItem(raw)
    if (item) {
      accepted.push(item)
    } else {
      invalid += 1
    }
  }

  // 截断与不合规是两回事：截断属于「合规但超量」，日志措辞要分开。
  const truncated = rawItems.length > LIMITS.maxItems ? rawItems.length - LIMITS.maxItems : 0

  return { valid: true, items: accepted, invalid, truncated }
}

function createRuntimeError(message, category) {
  const error = new Error(message)
  error.code = category
  return error
}

// AbortError 语义：宿主主动中止（停用/重载），调用方应静默跳过而不是判扩展失败。
function createAbortError(message) {
  const error = new Error(message)
  error.name = 'AbortError'
  error.code = ERROR_CATEGORIES.ABORT
  return error
}

export async function createExtensionRuntime(meta) {
  const source = await invoke('read_extension_entry', { id: meta.id })
  const extensionUrl = URL.createObjectURL(new Blob([source], { type: 'text/javascript' }))
  const workerSource = `${WORKER_PREFIX}\n${WORKER_SUFFIX.replace('__QC_EXTENSION_URL__', extensionUrl)}`
  const blob = new Blob([workerSource], { type: 'text/javascript' })
  const workerUrl = URL.createObjectURL(blob)
  let worker
  try {
    worker = new Worker(workerUrl, { type: 'module' })
  } catch (error) {
    // Worker 创建失败时两个 blob URL 都无人持有，必须立即回收。
    URL.revokeObjectURL(workerUrl)
    URL.revokeObjectURL(extensionUrl)
    throw error
  }

  return new ExtensionRuntime(meta, worker, workerUrl, extensionUrl)
}

class ExtensionRuntime {
  constructor(meta, worker, workerUrl, extensionUrl) {
    this.id = meta.id
    this.worker = worker
    this.workerUrl = workerUrl
    this.extensionUrl = extensionUrl
    this.pending = new Map()
    this.sequence = 0
    this.dead = false
    this.disposed = false
    // 冷启动期间（尚无任何成功响应）使用更宽的首次超时预算。
    this.hasResponded = false
    // 连续运行级失败计数：任一成功响应清零，达到阈值才停用。
    this.failureCount = 0

    this.worker.addEventListener('message', (event) => this.handleMessage(event))
    this.worker.addEventListener('error', () => this.fail('Worker 运行出错'))
  }

  handleMessage(event) {
    const message = event.data || {}

    if (message.type === MESSAGE_TYPES.LOG) {
      const args = Array.isArray(message.args) ? message.args : []
      console.info(`[extension:${this.id}]`, ...args)
      return
    }

    if (message.type !== MESSAGE_TYPES.RESULT) {
      return
    }

    const pending = this.pending.get(message.requestId)
    if (!pending) {
      return
    }
    this.pending.delete(message.requestId)
    clearTimeout(pending.timer)

    if (message.error) {
      const category = message.category === ERROR_CATEGORIES.RUNTIME
        ? ERROR_CATEGORIES.RUNTIME
        : ERROR_CATEGORIES.ITEM
      pending.reject(createRuntimeError(String(message.error), category))
      // 运行级错误（入口加载失败等）说明 Worker 已不可用，才停用扩展。
      if (category === ERROR_CATEGORIES.RUNTIME) {
        this.fail(String(message.error))
      }
      return
    }

    const batch = validateBatch(message.items)
    if (!batch.valid) {
      console.error(`[extension:${this.id}] 识别结果不是数组，已丢弃`)
    } else {
      if (batch.invalid > 0) {
        console.error(`[extension:${this.id}] 识别结果有 ${batch.invalid} 项不合规，已丢弃`)
      }
      if (batch.truncated > 0) {
        console.warn(
          `[extension:${this.id}] 识别结果超过 ${LIMITS.maxItems} 项上限，已截断 ${batch.truncated} 项`
        )
      }
    }
    this.hasResponded = true
    this.failureCount = 0
    pending.resolve(batch.items)
  }

  // 供调度层判断 Worker 是否已彻底不可用（区别于单次超时）。
  get unavailable() {
    return this.dead
  }

  // 先结算所有 pending（清定时器 + reject），任何退出路径都必须走这里，
  // 否则 await 方会永久挂起。
  settlePending(reason) {
    const pending = Array.from(this.pending.values())
    this.pending.clear()
    for (const entry of pending) {
      clearTimeout(entry.timer)
      entry.reject(createAbortError(reason))
    }
  }

  request(type, context) {
    if (this.dead) {
      return Promise.reject(createRuntimeError('扩展 Worker 不可用', ERROR_CATEGORIES.RUNTIME))
    }

    const requestId = `${this.id}-${++this.sequence}`
    const timeoutMs = this.hasResponded
      ? LIMITS.identifyTimeoutMs
      : LIMITS.identifyFirstTimeoutMs

    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        if (!this.pending.has(requestId)) {
          return
        }
        // 超时只打断当前请求；连续超时才说明 Worker 真的不健康。
        this.pending.delete(requestId)
        this.failureCount += 1
        reject(createRuntimeError(`扩展响应超时（${timeoutMs}ms）`, ERROR_CATEGORIES.RUNTIME))
        if (this.failureCount >= LIMITS.identifyFailureThreshold) {
          this.fail(`连续 ${this.failureCount} 次扩展响应失败`)
        }
      }, timeoutMs)

      this.pending.set(requestId, { resolve, reject, timer })
      try {
        this.worker.postMessage({ type, requestId, context })
      } catch (error) {
        this.pending.delete(requestId)
        clearTimeout(timer)
        // postMessage 失败意味着 Worker 通道已断，属于运行级错误。
        reject(createRuntimeError(
          String((error && error.message) || error),
          ERROR_CATEGORIES.RUNTIME
        ))
      }
    })
  }

  identify(context) {
    return this.request(MESSAGE_TYPES.IDENTIFY, context)
  }

  // 预检：只加载入口模块并校验导出约定，不调用 identify，不产生业务副作用。
  probe() {
    return this.request(MESSAGE_TYPES.PROBE, {})
  }

  fail(reason) {
    if (this.dead) {
      return
    }
    this.dead = true
    this.settlePending(reason)
    this.dispose(reason)
  }

  dispose(reason = '扩展已停用') {
    this.dead = true
    this.settlePending(reason)
    if (this.disposed) {
      return
    }
    this.disposed = true
    try {
      this.worker.terminate()
    } catch (_) {
      // ignore
    }
    try {
      URL.revokeObjectURL(this.workerUrl)
    } catch (_) {
      // ignore
    }
    try {
      URL.revokeObjectURL(this.extensionUrl)
    } catch (_) {
      // ignore
    }
  }
}
