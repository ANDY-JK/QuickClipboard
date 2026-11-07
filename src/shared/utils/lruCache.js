// 有界 LRU：同时支持「条数上限」和「估算字节上限」，
// 避免单条体积很大的数据（文件列表 JSON、拖拽预览图标）突破内存预算。
export function createLruCache({
  maxEntries = 500,
  maxBytes = Infinity,
  estimateBytes = null,
} = {}) {
  const entries = new Map()
  let totalBytes = 0

  const measure = (key, value) => {
    if (!estimateBytes || !Number.isFinite(maxBytes)) return 0
    const bytes = Number(estimateBytes(value, key))
    return Number.isFinite(bytes) && bytes > 0 ? bytes : 0
  }

  const removeEntry = (key) => {
    const entry = entries.get(key)
    if (!entry) return false
    entries.delete(key)
    totalBytes -= entry.bytes
    if (totalBytes < 0) totalBytes = 0
    return true
  }

  const evictOverflow = () => {
    while (entries.size > maxEntries || (totalBytes > maxBytes && entries.size > 0)) {
      const oldestKey = entries.keys().next().value
      removeEntry(oldestKey)
    }
  }

  return {
    get(key) {
      if (!entries.has(key)) return undefined
      const entry = entries.get(key)
      // 命中后移到队尾，保证最近使用的数据最后被淘汰。
      entries.delete(key)
      entries.set(key, entry)
      return entry.value
    },
    has(key) {
      return entries.has(key)
    },
    set(key, value) {
      const bytes = measure(key, value)
      // 单条就超出预算的条目直接不缓存，防止把整个缓存挤空。
      if (bytes > maxBytes) {
        removeEntry(key)
        return
      }
      removeEntry(key)
      entries.set(key, { value, bytes })
      totalBytes += bytes
      evictOverflow()
    },
    delete(key) {
      return removeEntry(key)
    },
    clear() {
      entries.clear()
      totalBytes = 0
    },
    get size() {
      return entries.size
    },
    get bytes() {
      return totalBytes
    },
  }
}
