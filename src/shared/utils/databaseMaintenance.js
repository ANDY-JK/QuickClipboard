// 手动压缩数据库期间后端会短暂锁库，其他 DB 命令统一返回
//「数据库正在维护（压缩中），请稍后重试」。列表/收藏刷新是非用户主动操作，
// 命中时静默重试一次，避免主窗口弹出一串错误。
const MAINTENANCE_PATTERN = /数据库正在维护|压缩中|under maintenance|compacting/i

export function isDatabaseMaintenanceError(error) {
  const raw = typeof error === 'string' ? error : error?.message
  return MAINTENANCE_PATTERN.test(raw || '')
}

export async function retryOnDatabaseMaintenance(task, delayMs = 1200) {
  try {
    return await task()
  } catch (error) {
    if (!isDatabaseMaintenanceError(error)) throw error
    await new Promise((resolve) => setTimeout(resolve, delayMs))
    return task()
  }
}

/**
 * 维护窗口结束后的有界退避补刷（默认 1.5s / 3s / 6s / 12s，共 4 次）。
 * 调用方在命中维护错误时 schedule()，在刷新成功且无残留标记时 cancel()。
 * 已有进行中的加载时只顺延、不消耗重试次数。
 */
export function createMaintenanceRetryScheduler({
  run,
  shouldDefer = () => false,
  baseDelay = 1500,
  maxAttempts = 4,
  onAttempt = null,
} = {}) {
  let timer = null
  let attempt = 0

  function cancel() {
    if (timer) {
      clearTimeout(timer)
      timer = null
    }
    attempt = 0
  }

  function schedule() {
    if (timer || attempt >= maxAttempts) {
      return
    }

    const delay = baseDelay * (2 ** attempt)
    timer = setTimeout(async () => {
      timer = null
      if (shouldDefer()) {
        schedule()
        return
      }
      attempt += 1
      if (onAttempt) {
        onAttempt(attempt)
      }
      try {
        await run()
      } catch (error) {
        console.error('维护期补刷失败:', error)
      }
    }, delay)
  }

  return { schedule, cancel }
}
