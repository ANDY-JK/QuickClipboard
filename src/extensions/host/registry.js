// 扩展注册表：负责元数据加载、按 contentTypes 过滤、调度识别与结果缓存。
// React 列表只读取这里发布的识别结果，识别过程完全不阻塞首屏。

import { invoke } from '@tauri-apps/api/core'
import { proxy } from 'valtio'
import { createExtensionRuntime } from './runtime'
import { ERROR_CATEGORIES, LIMITS } from '../protocol'

export const extensionsStore = proxy({
  enabled: true,
  loaded: false,
  loading: false,
  revision: 0,
  extensions: [],
  // extensionId -> 错误原因（运行级）
  errors: {},
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

function notifyItem(itemId) {
  const listeners = resultSubscribers.get(itemId)
  if (!listeners) return
  for (const listener of Array.from(listeners)) listener()
}

function forgetItemResult(itemId) {
  if (results.delete(itemId)) {
    notifyItem(itemId)
  }
}

function clearResults() {
  results.clear()
  for (const itemId of Array.from(resultSubscribers.keys())) {
    notifyItem(itemId)
  }
}

const runtimes = new Map()
const runtimePromises = new Map()
const failedExtensions = new Set()
const identifyCache = new Map()
// (extensionId:itemId) -> itemId，用于容量淘汰时同步清理该条目的结果
const cacheItemIds = new Map()
const cacheRefCounts = new Map()
const MAX_IDENTIFY_CACHE_ENTRIES = 2000
let initPromise = null
let generation = 0

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

export function initExtensions() {
  if (initPromise) {
    return initPromise
  }

  initPromise = (async () => {
    extensionsStore.loading = true
    try {
      const list = await invoke('list_extensions')
      extensionsStore.extensions = Array.isArray(list) ? list : []
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

export async function reloadExtensions() {
  disposeAll('扩展已重载')
  clearExtensionResults()
  failedExtensions.clear()
  extensionsStore.errors = {}
  initPromise = null
  await initExtensions()
  extensionsStore.revision += 1
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

  // 回收该扩展的识别缓存，并同步撤掉受影响条目里已发布的 chip。
  for (const [key, itemId] of Array.from(cacheItemIds)) {
    if (!key.startsWith(`${id}:`)) continue
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

  extensionsStore.extensions = extensionsStore.extensions.filter((meta) => meta.id !== id)
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

function markExtensionError(extensionId, reason) {
  failedExtensions.add(extensionId)
  extensionsStore.errors[extensionId] = reason
  const runtime = runtimes.get(extensionId)
  if (runtime) {
    runtime.dispose()
    runtimes.delete(extensionId)
  }
  console.warn(`[extension:${extensionId}] 已停用:`, reason)
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
    if (currentGeneration !== generation || !extensionsStore.enabled) {
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
      tasks.push({ meta, item, itemId, key })
    }
  }

  if (tasks.length === 0) {
    return
  }

  const taskGeneration = generation
  await Promise.all(
    tasks.map((task) =>
      enqueue(async () => {
        const chips = await identifyOne(task.meta, task.item)
        // 重载/停用后旧代际任务的结果不再写入缓存或发布。
        if (taskGeneration !== generation) return
        cacheResult(task.key, task.itemId, chips)
        if (extensionsStore.enabled) {
          publishItem(task.itemId)
        }
      }, taskGeneration).catch((error) => {
        if (error?.code !== ERROR_CATEGORIES.ABORT) {
          console.warn('扩展识别任务失败:', error)
        }
      })
    )
  )
}
