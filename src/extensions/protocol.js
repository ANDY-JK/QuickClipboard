// 扩展协议常量（MVP 定稿）
//
// 扩展目录结构：<应用数据目录>/extensions/<id>/manifest.json + <entry>.js

// 入口模块导出约定：优先命名导出 `extension`，未提供时回退默认导出。
//   `export const extension = { id, identify(ctx) }`
//   `export default { id, identify(ctx) }`
// 两者都没有时该扩展判定失败，并在设置页给出「未导出 extension 对象」的错误。

export const MESSAGE_TYPES = {
  IDENTIFY: 'identify',
  RESULT: 'result',
  LOG: 'log',
}

// 错误类别：决定「单条失败」还是「运行级失败」。
// 只有运行级失败才允许停用整个扩展。
export const ERROR_CATEGORIES = {
  ITEM: 'ITEM_ERROR',
  RUNTIME: 'RUNTIME_ERROR',
  // 宿主主动中止（停用/重载扩展时结算在飞请求），不应当作扩展故障。
  ABORT: 'ABORT_ERROR',
}

export const LIMITS = {
  // 单次识别超时（毫秒）。超时只 reject 当前请求，不再终止 Worker。
  identifyTimeoutMs: 1200,
  // 冷启动预算：Worker 首次识别要等入口模块 import() 完成，单独放宽。
  identifyFirstTimeoutMs: 4000,
  // 连续运行级失败达到该次数才降级停用扩展（任一成功响应清零）。
  identifyFailureThreshold: 3,
  // 单次识别返回的条目上限
  maxItems: 5,
  maxKindLength: 32,
  maxLabelLength: 40,
  maxDetailLength: 200,
  // 单批送入识别的条目上限（需覆盖大窗口下可见行数 + 预渲染行数）
  maxBatchItems: 40,
  // 同时运行的 Worker 上限
  maxConcurrentWorkers: 2,
  // 传给扩展的正文最大长度，避免大体积内容跨线程拷贝
  maxContextContentLength: 2000,
}
