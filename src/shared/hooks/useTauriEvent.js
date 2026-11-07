import { useEffect, useRef } from 'react'
import { listen } from '@tauri-apps/api/event'

/**
 * 注册 Tauri 事件监听。
 * - handler 存在 ref 中，回调里读到的最新状态（activeTab / 语言等）不会导致监听重建；
 * - listen 是异步的，组件在注册完成前卸载时会在 resolve 后立即 unlisten，避免泄漏与竞态。
 *
 * @param {string} eventName 事件名
 * @param {(event: import('@tauri-apps/api/event').Event) => void} handler 事件回调
 * @param {boolean} [enabled=true] 是否启用
 */
export function useTauriEvent(eventName, handler, enabled = true) {
  const handlerRef = useRef(handler)
  handlerRef.current = handler

  useEffect(() => {
    if (!enabled) return undefined

    let disposed = false
    let unlisten = null

    listen(eventName, (event) => {
      if (disposed) return
      handlerRef.current?.(event)
    })
      .then((unlistenFn) => {
        if (disposed) {
          unlistenFn()
          return
        }
        unlisten = unlistenFn
      })
      .catch((error) => {
        console.error(`监听事件 ${eventName} 失败:`, error)
      })

    return () => {
      disposed = true
      if (unlisten) {
        unlisten()
        unlisten = null
      }
    }
  }, [eventName, enabled])
}
