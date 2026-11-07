import { proxy } from 'valtio'
import { listen } from '@tauri-apps/api/event'
import { 
  getClipboardHistory, 
  getClipboardTotalCount,
  deleteClipboardItem as apiDeleteItem,
  clearClipboardHistory as apiClearHistory,
  pasteClipboardItem as apiPasteClipboardItem
} from '@shared/api'
import {
  createMaintenanceRetryScheduler,
  isDatabaseMaintenanceError,
  retryOnDatabaseMaintenance
} from '@shared/utils/databaseMaintenance'
import { forgetItems, clearExtensionResults } from '@/extensions/host/registry'

listen('paste-count-updated', (event) => {
  const id = event.payload
  for (const key of Object.keys(clipboardStore.items)) {
    const item = clipboardStore.items[key]
    if (item && item.id === id) {
      clipboardStore.items[key] = { ...item, paste_count: (item.paste_count || 0) + 1 }
      break
    }
  }
})

const CACHE_WINDOW_SIZE = 120
const CACHE_BUFFER = 40
const MAX_CONCURRENT_RANGE_LOADS = 2
const rangeLoadQueue = []
const activeRangeLoadKeys = new Set()
let activeRangeLoadCount = 0
let clipboardRequestVersion = 0

// 数据库维护窗口结束后的有界补刷（1.5s / 3s / 6s / 12s，共 4 次），
// 定时逻辑抽到 shared/utils/databaseMaintenance，与收藏列表共用。
const maintenanceRetry = createMaintenanceRetryScheduler({
  run: () => initClipboardItems(),
  shouldDefer: () => clipboardStore.loading,
  onAttempt: (attempt) => {
    console.info(`数据库维护中，第 ${attempt} 次自动刷新剪贴板列表`)
  },
})

function nextClipboardRequestVersion() {
  clipboardRequestVersion += 1
  return clipboardRequestVersion
}

function isClipboardRequestCurrent(version, filter, contentType, pasteStatus) {
  return version === clipboardRequestVersion
    && clipboardStore.filter === filter
    && clipboardStore.contentType === contentType
    && clipboardStore.pasteStatus === pasteStatus
}

// 供列表组件卸载时调用，避免定时器在窗口销毁后继续触发后台请求。
export function cancelClipboardMaintenanceRetry() {
  maintenanceRetry.cancel()
}

// 剪贴板 Store
export const clipboardStore = proxy({
  items: {}, 
  totalCount: 0,
  filter: '',
  contentType: 'all',
  pasteStatus: 'all',
  selectedIds: new Set(),
  selectedEntries: [],
  isMultiSelectMode: false,
  selectionAnchorIndex: null,
  loading: false,
  error: null,
  loadingRanges: new Set(),
  loadedRanges: [],
  hasLoadedOnce: false,
  pendingRefresh: false,
  currentViewRange: { start: 0, end: 50 }, 

  // 已加载索引重建为连续区间，用于计算“真正缺失”的分页范围。
  rebuildLoadedRanges() {
    const indices = Object.keys(this.items)
      .map(key => Number(key))
      .filter(Number.isInteger)
      .sort((a, b) => a - b)

    const ranges = []
    for (const index of indices) {
      const lastRange = ranges[ranges.length - 1]
      if (lastRange && index === lastRange[1] + 1) {
        lastRange[1] = index
      } else {
        ranges.push([index, index])
      }
    }
    this.loadedRanges = ranges
  },

  // 返回 [start,end] 中尚未加载且未在加载中的子区间。
  getMissingRanges(startIndex, endIndex) {
    const blockedRanges = []
    for (const range of this.loadedRanges) {
      blockedRanges.push(range)
    }
    for (const rangeKey of this.loadingRanges) {
      const [start, end] = rangeKey.split('-').map(Number)
      if (Number.isInteger(start) && Number.isInteger(end)) {
        blockedRanges.push([start, end])
      }
    }
    blockedRanges.sort((a, b) => a[0] - b[0])

    const missing = []
    let cursor = startIndex
    for (const [start, end] of blockedRanges) {
      if (end < cursor) continue
      if (start > endIndex) break
      if (start > cursor) {
        missing.push([cursor, Math.min(start - 1, endIndex)])
      }
      cursor = Math.max(cursor, end + 1)
      if (cursor > endIndex) break
    }
    if (cursor <= endIndex) {
      missing.push([cursor, endIndex])
    }
    return missing
  },

  // 设置指定范围的数据
  setItemsInRange(startIndex, items) {
    items.forEach((item, offset) => {
      this.items[startIndex + offset] = item
    })
    this.trimCache()
    this.rebuildLoadedRanges()
  },
  
  updateViewRange(startIndex, endIndex) {
    const prev = this.currentViewRange
    if (Math.abs(prev.start - startIndex) > 10 || Math.abs(prev.end - endIndex) > 10) {
      this.currentViewRange = { start: startIndex, end: endIndex }
      this.trimCache()
    }
  },
  
  trimCache() {
    const itemCount = Object.keys(this.items).length
    if (itemCount <= CACHE_WINDOW_SIZE) return
    
    const { start, end } = this.currentViewRange
    const center = Math.floor((start + end) / 2)
    const keepStart = Math.max(0, center - CACHE_BUFFER)
    const keepEnd = Math.min(this.totalCount - 1, center + CACHE_BUFFER)
    
    let removed = false
    for (const key of Object.keys(this.items)) {
      const index = parseInt(key, 10)
      if (index < keepStart || index > keepEnd) {
        delete this.items[key]
        removed = true
      }
    }

    if (removed) {
      this.rebuildLoadedRanges()
    }
  },

  getItem(index) {
    return this.items[index]
  },
  
  // 检查指定索引是否已加载
  hasItem(index) {
    return index in this.items
  },

  findLoadedItemIndex(id) {
    const entry = Object.entries(this.items)
      .map(([key, value]) => [parseInt(key, 10), value])
      .find(([, value]) => value?.id === id)
    return Number.isInteger(entry?.[0]) ? entry[0] : null
  },

  getLoadedItemById(id) {
    const index = this.findLoadedItemIndex(id)
    return Number.isInteger(index) ? this.items[index] : null
  },

  setFavoriteId(id, favoriteId) {
    const index = this.findLoadedItemIndex(id)
    if (!Number.isInteger(index) || !this.items[index]) return
    this.items[index] = { ...this.items[index], favorite_id: favoriteId || null }
  },

  clearFavoriteIds(favoriteIds) {
    const favoriteIdSet = new Set(favoriteIds.filter(Boolean))
    if (!favoriteIdSet.size) return

    for (const [key, item] of Object.entries(this.items)) {
      if (item?.favorite_id && favoriteIdSet.has(item.favorite_id)) {
        this.items[key] = { ...item, favorite_id: null }
      }
    }
  },

  addItem(item) {
    if (!item?.id) {
      return
    }

    // 本地插入，避免清空 items 造成整表回退到骨架屏。
    if (Number.isInteger(this.findLoadedItemIndex(item.id))) {
      return
    }
    this.insertLoadedItemAt(item, 0, this.totalCount + 1)
  },
  
  // 删除项
  removeItem(id) {
    this.removeItems([id])
  },

  removeItems(ids) {
    const removeIdSet = new Set(ids.filter(Boolean))
    if (!removeIdSet.size) {
      return
    }

    const entries = Object.entries(this.items)
      .map(([key, item]) => [parseInt(key, 10), item])
      .filter(([, item]) => item)
      .sort((a, b) => a[0] - b[0])

    let removedCount = 0
    const nextItems = {}
    for (const [index, item] of entries) {
      if (removeIdSet.has(item.id)) {
        removedCount += 1
        continue
      }
      nextItems[index - removedCount] = item
    }

    if (removedCount === 0) {
      return
    }

    this.items = nextItems
    this.totalCount = Math.max(0, this.totalCount - removedCount)
    this.rebuildLoadedRanges()
    // 删除条目后同步清掉扩展识别结果，避免缓存与 chip 残留。
    forgetItems(Array.from(removeIdSet))

    if (this.selectionAnchorIndex != null) {
      this.selectionAnchorIndex = Math.max(0, this.selectionAnchorIndex - removedCount)
    }
  },

  moveLoadedItem(fromIndex, toIndex) {
    if (fromIndex === toIndex) {
      return true
    }

    const start = Math.min(fromIndex, toIndex)
    const end = Math.max(fromIndex, toIndex)
    const rangeItems = []

    for (let index = start; index <= end; index += 1) {
      if (!this.hasItem(index)) {
        return false
      }
      rangeItems.push(this.items[index])
    }

    const movedItem = rangeItems[fromIndex - start]
    if (!movedItem) {
      return false
    }

    rangeItems.splice(fromIndex - start, 1)
    rangeItems.splice(toIndex - start, 0, movedItem)

    rangeItems.forEach((item, offset) => {
      this.items[start + offset] = item
    })

    if (this.selectionAnchorIndex != null && this.selectionAnchorIndex >= start && this.selectionAnchorIndex <= end) {
      if (this.selectionAnchorIndex === fromIndex) {
        this.selectionAnchorIndex = toIndex
      } else if (fromIndex < toIndex && this.selectionAnchorIndex > fromIndex && this.selectionAnchorIndex <= toIndex) {
        this.selectionAnchorIndex -= 1
      } else if (fromIndex > toIndex && this.selectionAnchorIndex >= toIndex && this.selectionAnchorIndex < fromIndex) {
        this.selectionAnchorIndex += 1
      }
    }

    if (this.selectedEntries.length > 0) {
      this.selectedEntries = this.selectedEntries
        .map(entry => {
          if (entry.index === fromIndex) {
            return { ...entry, index: toIndex }
          }
          if (fromIndex < toIndex && entry.index > fromIndex && entry.index <= toIndex) {
            return { ...entry, index: entry.index - 1 }
          }
          if (fromIndex > toIndex && entry.index >= toIndex && entry.index < fromIndex) {
            return { ...entry, index: entry.index + 1 }
          }
          return entry
        })
    }

    return true
  },

  insertLoadedItemAt(item, insertIndex, totalCount) {
    if (!item || !Number.isInteger(insertIndex) || insertIndex < 0 || insertIndex > totalCount) {
      return false
    }

    const nextItems = {}
    const oldEntry = Object.entries(this.items)
      .map(([key, value]) => [parseInt(key, 10), value])
      .find(([, value]) => value?.id === item.id)
    const oldIndex = oldEntry?.[0]
    const isMovingLoadedItem = Number.isInteger(oldIndex)
    const effectiveTotalCount = isMovingLoadedItem ? this.totalCount : totalCount
    const entries = Object.entries(this.items)
      .map(([key, value]) => [parseInt(key, 10), value])
      .filter(([, value]) => value)
      .sort((a, b) => b[0] - a[0])

    for (const [index, value] of entries) {
      if (value.id === item.id) {
        continue
      }

      let nextIndex = index
      if (isMovingLoadedItem) {
        if (oldIndex < insertIndex && index > oldIndex && index <= insertIndex) {
          nextIndex = index - 1
        } else if (oldIndex > insertIndex && index >= insertIndex && index < oldIndex) {
          nextIndex = index + 1
        }
      } else if (index >= insertIndex) {
        nextIndex = index + 1
      }

      if (nextIndex < effectiveTotalCount) {
        nextItems[nextIndex] = value
      }
    }

    nextItems[insertIndex] = item
    this.items = nextItems
    this.totalCount = effectiveTotalCount

    if (this.selectionAnchorIndex != null) {
      if (isMovingLoadedItem) {
        if (this.selectionAnchorIndex === oldIndex) {
          this.selectionAnchorIndex = insertIndex
        } else if (oldIndex < insertIndex && this.selectionAnchorIndex > oldIndex && this.selectionAnchorIndex <= insertIndex) {
          this.selectionAnchorIndex -= 1
        } else if (oldIndex > insertIndex && this.selectionAnchorIndex >= insertIndex && this.selectionAnchorIndex < oldIndex) {
          this.selectionAnchorIndex += 1
        }
      } else if (this.selectionAnchorIndex >= insertIndex) {
        this.selectionAnchorIndex += 1
      }
    }

    if (this.selectedEntries.length > 0) {
      this.selectedEntries = this.selectedEntries
        .map(entry => {
          if (isMovingLoadedItem) {
            if (entry.id === item.id || entry.index === oldIndex) {
              return { ...entry, index: insertIndex }
            }
            if (oldIndex < insertIndex && entry.index > oldIndex && entry.index <= insertIndex) {
              return { ...entry, index: entry.index - 1 }
            }
            if (oldIndex > insertIndex && entry.index >= insertIndex && entry.index < oldIndex) {
              return { ...entry, index: entry.index + 1 }
            }
            return entry
          }

          return entry.index >= insertIndex
            ? { ...entry, index: entry.index + 1 }
            : entry
        })
    }

    const { start, end } = this.currentViewRange
    if (isMovingLoadedItem) {
      this.currentViewRange = { start, end }
    } else if (insertIndex <= start) {
      this.currentViewRange = {
        start: start + 1,
        end: end + 1,
      }
    } else if (insertIndex <= end) {
      this.currentViewRange = {
        start,
        end: end + 1,
      }
    }

    this.rebuildLoadedRanges()
    return true
  },

  // 粘贴置顶：本地移动已加载项，避免清空 items 触发整表骨架屏。
  moveLoadedItemToTop(id) {
    const index = this.findLoadedItemIndex(id)
    if (!Number.isInteger(index)) {
      return false
    }

    const item = this.items[index]
    if (!item || item.is_pinned) {
      return false
    }

    const targetIndex = Object.keys(this.items)
      .map(key => Number(key))
      .filter(key => key < index && this.items[key]?.is_pinned)
      .length

    if (targetIndex === index) {
      return true
    }

    return this.insertLoadedItemAt(item, targetIndex, this.totalCount)
  },
  
  setFilter(value) {
    if (this.filter !== value) {
      nextClipboardRequestVersion()
      this.filter = value
      this.items = {}
      this.loadedRanges = []
      this.loadingRanges = new Set()
      this.exitMultiSelectMode()
    }
  },
  
  setContentType(value) {
    if (this.contentType !== value) {
      nextClipboardRequestVersion()
      this.contentType = value
      this.items = {}
      this.loadedRanges = []
      this.loadingRanges = new Set()
      this.exitMultiSelectMode()
    }
  },

  setPasteStatus(value) {
    if (this.pasteStatus !== value) {
      nextClipboardRequestVersion()
      this.pasteStatus = value
      this.items = {}
      this.loadedRanges = []
      this.loadingRanges = new Set()
      this.exitMultiSelectMode()
    }
  },
  
  enterMultiSelectMode(entry = null) {
    if (!entry?.id) return false
    this.isMultiSelectMode = true
    this.replaceSelection([entry])
    this.selectionAnchorIndex = entry.index
    return true
  },

  exitMultiSelectMode() {
    this.isMultiSelectMode = false
    this.selectedEntries = []
    this.selectedIds = new Set()
    this.selectionAnchorIndex = null
  },

  setSelectionAnchorIndex(index) {
    this.selectionAnchorIndex = typeof index === 'number' ? index : null
  },

  normalizeSelectedEntry(entry) {
    return {
      id: entry.id,
      index: entry.index,
      contentType: entry.contentType,
    }
  },

  hasSelectedId(id) {
    return this.selectedEntries.some(entry => entry.id === id)
  },

  replaceSelection(entries) {
    const uniqueEntries = []
    const seenIds = new Set()

    for (const entry of entries) {
      if (!entry?.id || seenIds.has(entry.id)) continue
      seenIds.add(entry.id)
      uniqueEntries.push(this.normalizeSelectedEntry(entry))
    }

    this.selectedEntries = uniqueEntries
    this.selectedIds = new Set(uniqueEntries.map(entry => entry.id))
  },

  toggleSelectedEntry(entry) {
    const normalizedEntry = this.normalizeSelectedEntry(entry)
    const exists = this.selectedEntries.some(selected => selected.id === normalizedEntry.id)
    if (exists) {
      if (this.selectedEntries.length === 1) {
        this.exitMultiSelectMode()
      } else {
        this.replaceSelection(this.selectedEntries.filter(selected => selected.id !== normalizedEntry.id))
      }
      return
    }

    this.replaceSelection([...this.selectedEntries, normalizedEntry])
  },

  selectRange(entries) {
    this.replaceSelection(entries)
  },

  getSelectedIds() {
    return this.selectedEntries.map(entry => entry.id)
  },

  toggleSelect(id) {
    if (!id) return
    this.toggleSelectedEntry({
      id,
      index: Number.MAX_SAFE_INTEGER,
      contentType: 'text',
    })
  },
  
  clearSelection() {
    this.selectedEntries = []
    this.selectedIds = new Set()
    this.selectionAnchorIndex = null
  },
  
  clearAll() {
    clearExtensionResults()
    this.items = {}
    this.totalCount = 0
    this.loadedRanges = []
    this.currentViewRange = { start: 0, end: 50 }
    this.exitMultiSelectMode()
  },
  
  // 记录正在加载的范围
  addLoadingRange(start, end) {
    this.loadingRanges.add(`${start}-${end}`)
  },
  
  removeLoadingRange(start, end) {
    this.loadingRanges.delete(`${start}-${end}`)
  },
  
  isRangeLoading(start, end) {
    return this.loadingRanges.has(`${start}-${end}`)
  }
})

// 合并/排队加载：重叠请求不再直接丢弃，而是只补齐尚未覆盖的子区间。
function buildRangeLoadKey(task) {
  return `${task.version}|${task.filter || ''}|${task.contentType}|${task.pasteStatus}|${task.start}-${task.end}`
}

function flushRangeLoadQueue() {
  while (activeRangeLoadCount < MAX_CONCURRENT_RANGE_LOADS && rangeLoadQueue.length > 0) {
    const task = rangeLoadQueue.shift()

    if (task.version !== clipboardRequestVersion) {
      task.resolve()
      continue
    }
    if (activeRangeLoadKeys.has(task.key)) {
      task.resolve()
      continue
    }

    activeRangeLoadKeys.add(task.key)
    activeRangeLoadCount += 1
    clipboardStore.addLoadingRange(task.start, task.end)

    runRangeLoadTask(task).finally(() => {
      activeRangeLoadKeys.delete(task.key)
      activeRangeLoadCount -= 1
      flushRangeLoadQueue()
    })
  }
}

function enqueueRangeLoad(task) {
  if (activeRangeLoadKeys.has(task.key)) {
    return null
  }
  const queued = rangeLoadQueue.find(entry => entry.key === task.key)
  if (queued) {
    return queued.promise
  }

  let resolve
  const promise = new Promise((res) => {
    resolve = res
  })
  rangeLoadQueue.push({ ...task, promise, resolve })
  flushRangeLoadQueue()
  return promise
}

async function runRangeLoadTask(task) {
  try {
    const result = await retryOnDatabaseMaintenance(() => getClipboardHistory({
      offset: task.start,
      limit: task.end - task.start + 1,
      contentType: task.contentType !== 'all' ? task.contentType : undefined,
      pasteStatus: task.pasteStatus,
      search: task.filter || undefined
    }))

    if (!isClipboardRequestCurrent(task.version, task.filter, task.contentType, task.pasteStatus)) {
      return
    }

    // 将数据按索引存储
    clipboardStore.setItemsInRange(task.start, result.items)

    // 更新总数
    if (result.total_count !== undefined) {
      clipboardStore.totalCount = result.total_count
    }
  } catch (err) {
    if (isClipboardRequestCurrent(task.version, task.filter, task.contentType, task.pasteStatus)) {
      if (isDatabaseMaintenanceError(err)) {
        // 压缩数据库期间锁库：保持现有列表，标记待刷新并安排退避补刷，避免弹错误。
        clipboardStore.pendingRefresh = true
        maintenanceRetry.schedule()
        console.info('数据库压缩中，稍后刷新剪贴板列表')
      } else {
        console.error(`加载范围 ${task.start}-${task.end} 失败:`, err)
        clipboardStore.error = err.message || '加载失败'
      }
    }
  } finally {
    if (isClipboardRequestCurrent(task.version, task.filter, task.contentType, task.pasteStatus)) {
      clipboardStore.removeLoadingRange(task.start, task.end)
    }
    task.resolve()
  }
}

// 加载指定范围的数据
export async function loadClipboardRange(startIndex, endIndex, requestContext = null) {
  const requestVersion = requestContext?.version ?? clipboardRequestVersion
  const requestFilter = requestContext?.filter ?? clipboardStore.filter
  const requestContentType = requestContext?.contentType ?? clipboardStore.contentType
  const requestPasteStatus = requestContext?.pasteStatus ?? clipboardStore.pasteStatus

  if (!isClipboardRequestCurrent(requestVersion, requestFilter, requestContentType, requestPasteStatus)) {
    return
  }

  if (clipboardStore.loading && !requestContext) {
    return
  }

  if (!Number.isInteger(startIndex) || !Number.isInteger(endIndex) || startIndex < 0 || endIndex < startIndex) {
    return
  }

  const maxIndex = clipboardStore.totalCount > 0 ? clipboardStore.totalCount - 1 : endIndex
  const safeEnd = Math.min(endIndex, maxIndex)
  if (safeEnd < startIndex) {
    return
  }

  const missingRanges = clipboardStore.getMissingRanges(startIndex, safeEnd)
  if (missingRanges.length === 0) {
    return
  }

  const tasks = []
  for (const [start, end] of missingRanges) {
    const task = {
      start,
      end,
      version: requestVersion,
      filter: requestFilter,
      contentType: requestContentType,
      pasteStatus: requestPasteStatus,
    }
    task.key = buildRangeLoadKey(task)
    const promise = enqueueRangeLoad(task)
    if (promise) {
      tasks.push(promise)
    }
  }

  await Promise.all(tasks)
}

export async function loadClipboardItems() {
  return await refreshClipboardHistory()
}

// 初始化加载
export async function initClipboardItems() {
  const requestVersion = nextClipboardRequestVersion()
  const requestFilter = clipboardStore.filter
  const requestContentType = clipboardStore.contentType
  const requestPasteStatus = clipboardStore.pasteStatus

  clipboardStore.loading = true
  clipboardStore.error = null
  let completed = false
  
  try {
    clipboardStore.items = {}
    clipboardStore.loadedRanges = []
    clipboardStore.loadingRanges = new Set()
    clipboardStore.pendingRefresh = false
    
    if (requestContentType !== 'all' || requestFilter || requestPasteStatus !== 'all') {
      const result = await retryOnDatabaseMaintenance(() => getClipboardHistory({
        offset: 0,
        limit: 100,
        contentType: requestContentType !== 'all' ? requestContentType : undefined,
        pasteStatus: requestPasteStatus,
        search: requestFilter || undefined
      }))

      if (!isClipboardRequestCurrent(requestVersion, requestFilter, requestContentType, requestPasteStatus)) {
        return
      }
      
      clipboardStore.totalCount = result.total_count
      clipboardStore.setItemsInRange(0, result.items)
    } else {
      const totalCount = await retryOnDatabaseMaintenance(() => getClipboardTotalCount())

      if (!isClipboardRequestCurrent(requestVersion, requestFilter, requestContentType, requestPasteStatus)) {
        return
      }

      clipboardStore.totalCount = totalCount
      
      if (totalCount > 0) {
        const endIndex = Math.min(99, totalCount - 1)
        await loadClipboardRange(0, endIndex, {
          version: requestVersion,
          filter: requestFilter,
          contentType: requestContentType,
          pasteStatus: requestPasteStatus,
        })
      }
    }
    completed = true
  } catch (err) {
    if (isClipboardRequestCurrent(requestVersion, requestFilter, requestContentType, requestPasteStatus)) {
      if (isDatabaseMaintenanceError(err)) {
        clipboardStore.pendingRefresh = true
        maintenanceRetry.schedule()
        console.info('数据库压缩中，稍后刷新剪贴板列表')
      } else {
        console.error('初始化剪贴板失败:', err)
        clipboardStore.error = err.message || '加载失败'
      }
    }
  } finally {
    if (isClipboardRequestCurrent(requestVersion, requestFilter, requestContentType, requestPasteStatus)) {
      clipboardStore.loading = false
      clipboardStore.hasLoadedOnce = true
      // 成功且没有残留的待刷新标记时，结束退避并清掉定时器；
      // 若范围加载仍命中维护中（pendingRefresh 为 true），保留下一次补刷。
      if (completed && !clipboardStore.pendingRefresh) {
        cancelClipboardMaintenanceRetry()
      }
    }
  }
}

// 刷新剪贴板历史
export async function refreshClipboardHistory() {
  return await initClipboardItems()
}

// 删除剪贴板项
export async function deleteClipboardItem(id) {
  try {
    await apiDeleteItem(id)
    clipboardStore.removeItem(id)
    return true
  } catch (err) {
    console.error('删除剪贴板项失败:', err)
    throw err
  }
}

// 清空剪贴板历史
export async function clearClipboardHistory() {
  try {
    await apiClearHistory()
    clipboardStore.clearAll()
    return true
  } catch (err) {
    console.error('清空剪贴板历史失败:', err)
    throw err
  }
}

// 粘贴剪贴板项
export async function pasteClipboardItem(id) {
  try {
    await apiPasteClipboardItem(id)
    return true
  } catch (err) {
    console.error('粘贴剪贴板项失败:', err)
    throw err
  }
}

