import { convertFileSrc, invoke } from '@tauri-apps/api/core'

const DEFAULT_MAX_SIDE = 512

// 同一 imageId/路径 + 档位的并发请求只发一次 IPC；滚动时多个 <img> 同时挂载可直接复用。
const inFlightRequests = new Map()
// 失败记忆：同一 key 失败后本会话不再请求，调用方静默降级到原图。
// 切换存储目录/导入数据会重载窗口（reloadAllWindows），模块状态随之清空。
const failedRequests = new Set()

function buildRequestKey({ imageId, path, maxSide }) {
  return `${imageId || ''}|${path || ''}|${maxSide}`
}

/**
 * 请求图片缩略图。
 * 返回后端 ThumbnailInfo（含 path / originalPath / width / height / fromCache / isOriginal 等），
 * 并补充 src（可直接给 <img> 用的 asset URL）。
 * 失败时抛出后端的可展示错误字符串；调用方应静默降级到原图。
 */
export async function getImageThumbnail({ imageId = null, path = null, maxSide = DEFAULT_MAX_SIDE } = {}) {
  const key = buildRequestKey({ imageId, path, maxSide })
  if (failedRequests.has(key)) {
    throw new Error('缩略图请求此前已失败，跳过')
  }

  const pending = inFlightRequests.get(key)
  if (pending) {
    return pending
  }

  const payload = { maxSide }
  if (imageId) payload.imageId = imageId
  if (path) payload.path = path

  const request = invoke('get_image_thumbnail', payload)
    .then((info) => ({
      ...info,
      src: convertFileSrc(info.path, 'asset'),
    }))
    .catch((error) => {
      failedRequests.add(key)
      throw error
    })
    .finally(() => {
      inFlightRequests.delete(key)
    })

  inFlightRequests.set(key, request)
  return request
}

/**
 * 清理缩略图磁盘缓存中超出上限的旧文件。
 * 返回 { removedFiles, removedBytes, totalFiles, totalBytes }，
 * 其中 total* 是清理前扫描到的总量。
 */
export async function cleanupThumbnailCache() {
  return invoke('cleanup_thumbnail_cache')
}
