// 扩展注册表：负责元数据加载、按 contentTypes 过滤、调度识别与结果缓存。
// React 列表只读取这里发布的识别结果，识别过程完全不阻塞首屏。

import { invoke } from '@tauri-apps/api/core'
import { emit, listen } from '@tauri-apps/api/event'
import { proxy } from 'valtio'
import { createExtensionRuntime } from './runtime'
import { ERROR_CATEGORIES, LIMITS } from '../protocol'

// 跨窗口同步事件：每个窗口是独立 JS 运行时，扩展注册表不能靠模块单例共享，
// 安装/卸载/重载后由变更方广播，其他窗口收到后重新扫描磁盘。
const REGISTRY_CHANGED_EVENT = 'qc-extensions-registry-changed'
const RUNTIME_ERROR_EVENT = 'qc-extensions-runtime-error'
// 用于过滤自己发出的广播（Tauri emit 会送达全部窗口，包括发送方自己）。
const WINDOW_ORIGIN = `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`

// 元数据展示上限：manifest 本身已有 64KB 体积上限，这里再收窄到 UI 可承受的量级，
// 防止异常清单把超长字符串直接灌进列表与 chip。
const META_LIMITS = {
  name: 80,
  version: 40,
  description: 200,
  error: 300,
}

function normalizeMeta(raw) {
  if (!raw || typeof raw !== 'object') {
    return null
  }

  const id = typeof raw.id === 'string' ? raw.id.trim() : ''
  if (!id) {
    return null
  }

  const contentTypes = Array.isArray(raw.contentTypes)
    ? raw.contentTypes.filter((type) => typeof type === 'string')
    : []

  return {
    ...raw,
    id,
    name: typeof raw.name === 'string' ? raw.name.trim().slice(0, META_LIMITS.name) : '',
    version: typeof raw.version === 'string' ? raw.version.trim().slice(0, META_LIMITS.version) : '',
    description: typeof raw.description === 'string'
      ? raw.description.trim().slice(0, META_LIMITS.description)
      : '',
    error: raw.error == null ? null : String(raw.error).slice(0, META_LIMITS.error),
    contentTypes,
    valid: Boolean(raw.valid),
  }
}

export const extensionsStore = proxy({
  enabled: true,
  loaded: false,
  loading: false,
  revision: 0,
  extensions: [],
  // extensionId -> 错误原因（运行级）
  errors: {},
  // 识别结果变更计数：结果缓存在模块级 Map，列表按插件筛选时靠它触发重算。
  resultsRevision: 0,
})

// 识别结果独立于 valtio store，按 itemId 精确订阅。
// 避免任何一条结果发布都让整张列表与所有 chip 重渲染。
const EMPTY_RESULTS = Object.freeze([])
const results = new Map()
const resultSubscribers = new Map()

export function subscribeExtensionResults(itemId, listener) {
  const key = String(itemId)
  let listeners = resultSubscribers.get(key)
  if (!listeners) {
    listeners = new Set()
    resultSubscribers.set(key, listeners)
  }
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
    if (listeners.size === 0) {
      resultSubscribers.delete(key)
    }
  }
}

export function getExtensionResults(itemId) {
  return results.get(String(itemId)) || EMPTY_RESULTS
}

function bumpResultsRevision() {
  extensionsStore.resultsRevision += 1
}

/**
 * 派生查询（插件筛选）：该扩展是否在 itemId 上产生了命中结果。
 * 只读识别缓存，供列表按插件过滤使用，不改变扩展协议。
 */
export function isItemIdentifiedBy(extensionId, itemId) {
  if (!extensionId) return false
  const cached = identifyCache.get(`${extensionId}:${String(itemId)}`)
  return Boolean(cached && cached.length > 0)
}

/**
 * 派生查询（插件筛选）：该扩展对某条目是否仍在识别中。
 * 未进缓存但扩展对条目适用时也视为待识别（调用方随后会触发 identifyItems）。
 */
export function isExtensionIdentifyPending(extensionId, item) {
  if (!extensionId || !extensionsStore.enabled) return false
  const itemId = String(item?.id ?? '')
  if (!itemId) return false
  const meta = extensionsStore.extensions.find((entry) => entry.id === extensionId)
  if (!meta || !meta.valid || failedExtensions.has(meta.id)) return false
  const key = `${extensionId}:${itemId}`
  if (pendingIdentifies.has(key)) return true
  if (identifyCache.has(key)) return false
  return isApplicable(meta, item)
}

function notifyItem(itemId) {
  const listeners = resultSubscribers.get(itemId)
  if (!listeners) return
  for (const listener of Array.from(listeners)) listener()
}

function forgetItemResult(itemId) {
  if (results.delete(itemId)) {
    notifyItem(itemId)
    bumpResultsRevision()
  }
}

function clearResults() {
  results.clear()
  for (const itemId of Array.from(resultSubscribers.keys())) {
    notifyItem(itemId)
  }
  bumpResultsRevision()
}

const runtimes = new Map()
const runtimePromises = new Map()
const failedExtensions = new Set()
const identifyCache = new Map()
// 正在识别的 (extensionId:itemId)，供插件筛选区分“识别中”和“无命中”。
const pendingIdentifies = new Set()
// (extensionId:itemId) -> itemId，用于容量淘汰时同步清理该条目的结果
const cacheItemIds = new Map()
const cacheRefCounts = new Map()
const MAX_IDENTIFY_CACHE_ENTRIES = 2000
let initPromise = null
let generation = 0
// 跨窗口同步：监听器只注册一次；同一时间只允许一次远端触发的重载。
let syncSetupPromise = null
let remoteReloadQueued = false
let broadcastScheduled = false
let pendingBroadcastReason = 'reload'

let activeJobs = 0
const jobQueue = []

function createCancelError(reason) {
  const error = new Error(reason)
  error.name = 'AbortError'
  error.code = ERROR_CATEGORIES.ABORT
  return error
}

function cancelQueuedJobs(reason) {
  if (jobQueue.length === 0) {
    return
  }
  // 被取消的任务不会执行 finally，这里统一清掉它们的“识别中”标记。
  pendingIdentifies.clear()
  const queued = jobQueue.splice(0, jobQueue.length)
  for (const job of queued) {
    job.reject(createCancelError(reason))
  }
}

function pumpQueue() {
  while (activeJobs < LIMITS.maxConcurrentWorkers && jobQueue.length > 0) {
    const job = jobQueue.shift()
    // 重载/停用后，旧代际的排队任务直接作废，避免用新运行时重新执行旧请求。
    if (job.generation !== generation) {
      job.reject(createCancelError('扩展运行时已重置'))
      continue
    }
    activeJobs += 1
    Promise.resolve()
      .then(job.task)
      .then(job.resolve, job.reject)
      .finally(() => {
        activeJobs -= 1
        pumpQueue()
      })
  }
}

function enqueue(task, jobGeneration = generation) {
  return new Promise((resolve, reject) => {
    jobQueue.push({ task, generation: jobGeneration, resolve, reject })
    pumpQueue()
  })
}

function clearIdentifyCache() {
  identifyCache.clear()
  cacheItemIds.clear()
  cacheRefCounts.clear()
  pendingIdentifies.clear()
}

function cacheResult(key, itemId, chips) {
  if (!identifyCache.has(key)) {
    cacheItemIds.set(key, itemId)
    cacheRefCounts.set(itemId, (cacheRefCounts.get(itemId) || 0) + 1)
  }
  identifyCache.set(key, chips)

  // FIFO 上限：超限淘汰最老的缓存项，并回收不再被任何扩展缓存的条目结果。
  while (identifyCache.size > MAX_IDENTIFY_CACHE_ENTRIES) {
    const oldestKey = identifyCache.keys().next().value
    const oldestItemId = cacheItemIds.get(oldestKey)
    identifyCache.delete(oldestKey)
    cacheItemIds.delete(oldestKey)
    if (!oldestItemId) continue
    const nextCount = (cacheRefCounts.get(oldestItemId) || 1) - 1
    if (nextCount <= 0) {
      cacheRefCounts.delete(oldestItemId)
      forgetItemResult(oldestItemId)
    } else {
      cacheRefCounts.set(oldestItemId, nextCount)
    }
  }
}

// 删除/清空条目时调用：清掉这些 itemId 的缓存与已发布结果，避免陈旧 chip 残留。
export function forgetItems(itemIds) {
  const targetIds = new Set(
    (Array.isArray(itemIds) ? itemIds : [itemIds])
      .filter((id) => id != null)
      .map((id) => String(id))
  )
  if (targetIds.size === 0) {
    return
  }

  for (const [key, cachedItemId] of cacheItemIds) {
    if (targetIds.has(cachedItemId)) {
      identifyCache.delete(key)
      cacheItemIds.delete(key)
    }
  }
  for (const itemId of targetIds) {
    cacheRefCounts.delete(itemId)
    forgetItemResult(itemId)
  }
}

export function clearExtensionResults() {
  clearIdentifyCache()
  clearResults()
}

function emitExtensionEvent(name, payload) {
  emit(name, payload).catch((error) => {
    // 事件权限缺失或窗口正在销毁时不应影响本地功能，只记录警告。
    console.warn(`扩展状态广播失败（${name}）:`, error)
  })
}

function broadcastRegistryChange(reason = 'reload') {
  pendingBroadcastReason = reason
  if (broadcastScheduled) return
  broadcastScheduled = true
  // 合并同一轮内的多次变更（例如卸载后紧接着重载），只广播一次。
  Promise.resolve().then(() => {
    broadcastScheduled = false
    const nextReason = pendingBroadcastReason
    pendingBroadcastReason = 'reload'
    emitExtensionEvent(REGISTRY_CHANGED_EVENT, {
      origin: WINDOW_ORIGIN,
      reason: nextReason,
    })
  })
}

function broadcastRuntimeError(extensionId, reason) {
  emitExtensionEvent(RUNTIME_ERROR_EVENT, { origin: WINDOW_ORIGIN, extensionId, reason })
}

async function handleRemoteRegistryChange() {
  if (remoteReloadQueued) return
  remoteReloadQueued = true
  try {
    // 远端广播对应磁盘上的真实变更，这里只重新扫描，不再回广播，避免窗口间循环触发。
    await reloadExtensions({ broadcast: false })
  } catch (error) {
    console.warn('同步其他窗口的扩展变更失败:', error)
  } finally {
    remoteReloadQueued = false
  }
}

function ensureExtensionSync() {
  if (syncSetupPromise) {
    return syncSetupPromise
  }

  syncSetupPromise = (async () => {
    await listen(REGISTRY_CHANGED_EVENT, (event) => {
      if (event?.payload?.origin === WINDOW_ORIGIN) return
      handleRemoteRegistryChange()
    })
    await listen(RUNTIME_ERROR_EVENT, (event) => {
      const payload = event?.payload || {}
      if (payload.origin === WINDOW_ORIGIN) return
      const extensionId = typeof payload.extensionId === 'string' ? payload.extensionId : ''
      const reason = typeof payload.reason === 'string' ? payload.reason : ''
      if (!extensionId || !reason) return
      // 其他窗口已经判定该扩展不可用，本窗口直接跟随，避免重复创建 Worker 再失败一次。
      applyExtensionError(extensionId, reason)
    })
  })().catch((error) => {
    syncSetupPromise = null
    console.warn('扩展状态同步监听器启动失败:', error)
  })

  return syncSetupPromise
}

export function initExtensions() {
  if (initPromise) {
    return initPromise
  }

  ensureExtensionSync()

  initPromise = (async () => {
    extensionsStore.loading = true
    try {
      const list = await invoke('list_extensions')
      const normalized = (Array.isArray(list) ? list : [])
        .map(normalizeMeta)
        .filter(Boolean)
      extensionsStore.extensions = normalized
    } catch (error) {
      extensionsStore.extensions = []
      console.warn('加载扩展列表失败:', error)
    } finally {
      extensionsStore.loading = false
      extensionsStore.loaded = true
    }
  })()

  return initPromise
}

export async function reloadExtensions({ broadcast = true, reason = 'reload' } = {}) {
  disposeAll('扩展已重载')
  clearExtensionResults()
  failedExtensions.clear()
  extensionsStore.errors = {}
  initPromise = null
  await initExtensions()
  extensionsStore.revision += 1
  if (broadcast) {
    broadcastRegistryChange(reason)
  }
}

/**
 * 卸载前释放单个扩展的 Worker 与缓存。
 *
 * 只影响这一个扩展：其他扩展已发布的结果保持不变，
 * 该扩展名下（含仍在排队）的任务靠代际检查自然作废即可。
 * 先 dispose 再删目录，避免 Windows 上文件仍被 Worker 占用而删不掉。
 */
export function disposeExtension(extensionId) {
  const id = String(extensionId)

  const runtime = runtimes.get(id)
  if (runtime) {
    runtime.dispose('扩展已卸载')
    runtimes.delete(id)
  }
  runtimePromises.delete(id)
  failedExtensions.delete(id)
  if (extensionsStore.errors[id] !== undefined) {
    delete extensionsStore.errors[id]
  }

  dropExtensionCache(id)
  extensionsStore.extensions = extensionsStore.extensions.filter((meta) => meta.id !== id)
}

// 回收某个扩展的识别缓存，并同步撤掉受影响条目里已发布的 chip。
function dropExtensionCache(extensionId) {
  for (const [key, itemId] of Array.from(cacheItemIds)) {
    if (!key.startsWith(`${extensionId}:`)) continue
    identifyCache.delete(key)
    cacheItemIds.delete(key)
    const nextCount = (cacheRefCounts.get(itemId) || 1) - 1
    if (nextCount <= 0) {
      cacheRefCounts.delete(itemId)
      forgetItemResult(itemId)
    } else {
      cacheRefCounts.set(itemId, nextCount)
    }
  }
}

export function syncExtensionsEnabled(enabled) {
  const next = enabled !== false
  if (extensionsStore.enabled === next) {
    return
  }

  extensionsStore.enabled = next
  extensionsStore.revision += 1

  if (!next) {
    disposeAll('扩展已停用')
    clearExtensionResults()
  } else {
    // 重新启用必须清除上次的失败记录，否则 failedExtensions 会把扩展永久挡在识别之外。
    // initPromise 不重置：扩展列表本身很少变化，避免每次开关都触发一次 invoke；
    // 需要感知新装扩展时走 reloadExtensions()。
    failedExtensions.clear()
    extensionsStore.errors = {}
    initExtensions()
  }
}

function disposeAll(reason = '扩展运行时已重置') {
  generation += 1
  cancelQueuedJobs(reason)
  for (const runtime of runtimes.values()) {
    runtime.dispose(reason)
  }
  runtimes.clear()
  runtimePromises.clear()
}

// 本地（含远端事件）统一的失败落点：标记失败、撤运行时、撤已发布 chip。
function applyExtensionError(extensionId, reason) {
  const text = String(reason == null ? '' : reason).slice(0, META_LIMITS.error)
  failedExtensions.add(extensionId)
  extensionsStore.errors[extensionId] = text
  const runtime = runtimes.get(extensionId)
  if (runtime) {
    runtime.dispose()
    runtimes.delete(extensionId)
  }
  dropExtensionCache(extensionId)
  console.warn(`[extension:${extensionId}] 已停用:`, text)
  return text
}

function markExtensionError(extensionId, reason) {
  const text = applyExtensionError(extensionId, reason)
  // 广播给其他窗口，让设置页（或主窗口）不必各自再踩一次同样的加载失败。
  broadcastRuntimeError(extensionId, text)
}

function isApplicable(meta, item) {
  const contentTypes = Array.isArray(meta.contentTypes) ? meta.contentTypes : []
  if (contentTypes.length === 0) {
    return true
  }

  const itemType = String(item.content_type || item.contentType || '')
  const tokens = itemType
    .split(',')
    .map((token) => token.trim())
    .filter(Boolean)

  return contentTypes.some((type) => tokens.includes(type))
}

function buildContext(item) {
  const content = typeof item.content === 'string' ? item.content : ''
  return {
    id: String(item.id ?? ''),
    contentType: String(item.content_type || item.contentType || ''),
    content: content.slice(0, LIMITS.maxContextContentLength),
    isPinned: Boolean(item.is_pinned),
  }
}

async function getRuntime(meta) {
  if (failedExtensions.has(meta.id)) {
    return null
  }

  const existing = runtimes.get(meta.id)
  if (existing) {
    return existing
  }

  const currentGeneration = generation
  let runtimePromise = runtimePromises.get(meta.id)
  if (!runtimePromise) {
    runtimePromise = createExtensionRuntime(meta)
    runtimePromises.set(meta.id, runtimePromise)
  }

  try {
    const runtime = await runtimePromise
    if (
      currentGeneration !== generation
      || !extensionsStore.enabled
      || failedExtensions.has(meta.id)
    ) {
      runtime.dispose()
      return null
    }
    runtimes.set(meta.id, runtime)
    return runtime
  } catch (error) {
    const reason = String((error && error.message) || error)
    markExtensionError(meta.id, reason)
    return null
  } finally {
    runtimePromises.delete(meta.id)
  }
}

function publishItem(itemId) {
  const chips = []
  for (const meta of extensionsStore.extensions) {
    const cached = identifyCache.get(`${meta.id}:${itemId}`)
    if (cached && cached.length > 0) {
      chips.push(...cached)
    }
  }
  if (chips.length > 0) {
    results.set(itemId, chips)
    notifyItem(itemId)
  }
}

async function identifyOne(meta, item) {
  const runtime = await getRuntime(meta)
  if (!runtime) {
    return []
  }

  try {
    const items = await runtime.identify(buildContext(item))
    return items.map((chip) => ({ extensionId: meta.id, ...chip }))
  } catch (error) {
    // 宿主主动中止（停用/重载）：只跳过该条目，不计入扩展失败。
    if (error?.code === ERROR_CATEGORIES.ABORT) {
      console.info(`[extension:${meta.id}] 识别请求已中止:`, error.message)
      return []
    }
    // 只有 Worker 已彻底不可用（入口加载失败 / 崩溃 / 连续超时）才停用扩展；
    // 单条内容触发的异常或单次超时只跳过该条目。
    if (error?.code === ERROR_CATEGORIES.RUNTIME && runtime.unavailable) {
      markExtensionError(meta.id, String((error && error.message) || error))
    } else {
      console.warn(`[extension:${meta.id}] 单条识别失败，已跳过该条目:`, error)
    }
    return []
  }
}

/**
 * 对一批条目执行识别。只处理前 LIMITS.maxBatchItems 条，
 * 并发上限由全局队列控制，结果按 (extensionId, itemId) 缓存。
 */
export async function identifyItems(items) {
  if (!extensionsStore.enabled || !Array.isArray(items) || items.length === 0) {
    return
  }

  if (!extensionsStore.loaded) {
    await initExtensions()
  }

  if (!extensionsStore.enabled) {
    return
  }

  const metas = extensionsStore.extensions.filter(
    (meta) => meta.valid && !failedExtensions.has(meta.id)
  )
  if (metas.length === 0) {
    return
  }

  const batch = items.slice(0, LIMITS.maxBatchItems)
  const tasks = []

  for (const item of batch) {
    const itemId = String(item.id ?? '')
    if (!itemId) continue

    for (const meta of metas) {
      if (!isApplicable(meta, item)) continue
      const key = `${meta.id}:${itemId}`
      if (identifyCache.has(key)) continue
      cacheResult(key, itemId, [])
      pendingIdentifies.add(key)
      tasks.push({ meta, item, itemId, key })
    }
  }

  if (tasks.length === 0) {
    return
  }

  // 任务入队后先通知一次：插件筛选视图据此从“无命中”切到“识别中”。
  bumpResultsRevision()

  const taskGeneration = generation
  await Promise.all(
    tasks.map((task) =>
      enqueue(async () => {
        try {
          const chips = await identifyOne(task.meta, task.item)
          // 重载/停用后旧代际任务的结果不再写入缓存或发布。
          if (taskGeneration !== generation) return
          cacheResult(task.key, task.itemId, chips)
          if (extensionsStore.enabled) {
            publishItem(task.itemId)
          }
        } finally {
          pendingIdentifies.delete(task.key)
        }
      }, taskGeneration).catch((error) => {
        if (error?.code !== ERROR_CATEGORIES.ABORT) {
          console.warn('扩展识别任务失败:', error)
        }
      })
    )
  )

  // 整批完成后再通知一次，插件筛选视图把本批命中项并入列表。
  bumpResultsRevision()
}

let preflightPromise = null

function releaseProbeRuntime(extensionId) {
  const runtime = runtimes.get(extensionId)
  if (!runtime) return
  runtime.dispose()
  runtimes.delete(extensionId)
}

/**
 * 设置页专用预检：逐个加载入口模块并校验导出约定（不调用 identify）。
 * 目的：让入口缺失、导出不合规、Worker 启动失败等问题在设置页立即可见，
 * 而不是等主窗口第一次识别时才静默失败。
 */
export function preflightExtensions() {
  if (preflightPromise) {
    return preflightPromise
  }
  preflightPromise = runPreflight().finally(() => {
    preflightPromise = null
  })
  return preflightPromise
}

async function runPreflight() {
  if (!extensionsStore.enabled) {
    return
  }
  if (!extensionsStore.loaded) {
    await initExtensions()
  }
  if (!extensionsStore.enabled) {
    return
  }

  const metas = extensionsStore.extensions.filter(
    (meta) => meta.valid && !failedExtensions.has(meta.id)
  )
  if (metas.length === 0) {
    return
  }

  const taskGeneration = generation
  await Promise.all(
    metas.map((meta) =>
      enqueue(async () => {
        if (taskGeneration !== generation || !extensionsStore.enabled) return
        const runtime = await getRuntime(meta)
        if (!runtime) return
        try {
          await runtime.probe()
          // 设置页只做加载检查，不产生识别需求；探活成功后立即释放 Worker，
          // 避免长期打开设置页时每个扩展都常驻一个 Worker。
          releaseProbeRuntime(meta.id)
        } catch (error) {
          if (error?.code === ERROR_CATEGORIES.ABORT) return
          // 只有 Worker 彻底不可用（入口加载失败/反复超时）才停用扩展并广播；
          // 单次超时只记录，避免慢机器上误停用。
          if (error?.code === ERROR_CATEGORIES.RUNTIME && runtime.unavailable) {
            markExtensionError(meta.id, String((error && error.message) || error))
          } else {
            console.warn(`[extension:${meta.id}] 预检未通过（不影响其他扩展）:`, error)
          }
        }
      }, taskGeneration).catch((error) => {
        if (error?.code !== ERROR_CATEGORIES.ABORT) {
          console.warn(`[extension:${meta.id}] 预检任务失败:`, error)
        }
      })
    )
  )
}
