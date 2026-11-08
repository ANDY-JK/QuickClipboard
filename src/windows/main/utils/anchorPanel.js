// 触发按钮与浮层的定位工具：FilterTypeDropdown 与 PluginFilterDropdown 共用。
// 不写死面板高度：先按视口剩余空间钳制 maxHeight，上方不足时整体翻转到按钮上方。

const DEFAULT_VIEWPORT_MARGIN = 4

export function computeAnchoredPanelStyle({
  triggerRect,
  mode = 'horizontal',
  minWidth = 136,
  maxHeight = 240,
  viewportMargin = DEFAULT_VIEWPORT_MARGIN,
}) {
  if (!triggerRect) {
    return null
  }

  const width = Math.min(
    Math.max(triggerRect.width, minWidth),
    Math.max(minWidth, window.innerWidth - viewportMargin * 2)
  )

  let left = mode === 'sidebar' ? triggerRect.right + viewportMargin : triggerRect.left
  if (left + width > window.innerWidth - viewportMargin) {
    left = Math.max(viewportMargin, window.innerWidth - width - viewportMargin)
  }

  const spaceBelow = window.innerHeight - triggerRect.bottom - viewportMargin
  const spaceAbove = triggerRect.top - viewportMargin
  const placeAbove = spaceBelow < maxHeight && spaceAbove > spaceBelow
  const availableHeight = Math.max(
    0,
    Math.min(maxHeight, (placeAbove ? spaceAbove : spaceBelow) - viewportMargin)
  )

  const style = {
    left: `${Math.round(left)}px`,
    width: `${Math.round(width)}px`,
    maxHeight: `${Math.round(availableHeight)}px`,
  }
  if (placeAbove) {
    style.bottom = `${Math.round(window.innerHeight - triggerRect.top + viewportMargin)}px`
  } else {
    style.top = `${Math.round(triggerRect.bottom + viewportMargin)}px`
  }
  return style
}
