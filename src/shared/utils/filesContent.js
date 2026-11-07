import { createLruCache } from './lruCache'

// 文件类内容的 JSON 解析在列表渲染、时间格式化和拖拽路径中反复出现，
// 统一走有界缓存，避免每个渲染路径各自 JSON.parse。
// 条数 500 + 约 8MB 字节预算双上限：单条很大的 content 不会挤占整个缓存。
const FILES_CONTENT_CACHE_MAX_ENTRIES = 500
const FILES_CONTENT_CACHE_MAX_BYTES = 8 * 1024 * 1024
const filesContentCache = createLruCache({
  maxEntries: FILES_CONTENT_CACHE_MAX_ENTRIES,
  maxBytes: FILES_CONTENT_CACHE_MAX_BYTES,
  // content 为 UTF-16 字符串，按 2 字节/字符保守估算。
  estimateBytes: (entry) => (entry?.contentLength || 0) * 2,
})

function resolveCacheKey(content, cacheKey) {
  const identity = cacheKey != null ? String(cacheKey) : ''
  return `${identity}:${content.length}`
}

export function parseFilesContent(content, cacheKey = null) {
  if (typeof content !== 'string' || !content.startsWith('files:')) {
    return null
  }

  const key = resolveCacheKey(content, cacheKey)
  const cached = filesContentCache.get(key)
  if (cached && cached.contentLength === content.length) {
    return cached.data
  }

  try {
    const data = JSON.parse(content.slice(6))
    filesContentCache.set(key, { contentLength: content.length, data })
    return data
  } catch {
    filesContentCache.set(key, { contentLength: content.length, data: null })
    return null
  }
}

export function hasMissingFile(content, cacheKey = null) {
  const data = parseFilesContent(content, cacheKey)
  return Boolean(data?.files?.some(file => file?.exists === false))
}

export function getFilesCount(content, cacheKey = null) {
  const data = parseFilesContent(content, cacheKey)
  return data?.files?.length || 0
}
