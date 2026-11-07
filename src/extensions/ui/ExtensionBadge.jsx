// 列表条目上的扩展识别结果 chip。
// 绝对定位在条目右下角，不参与文档流，因此不会改变行高或引起列表重排。

import { useCallback, useSyncExternalStore } from 'react'
import { useTranslation } from 'react-i18next'
import Tooltip from '@shared/components/common/Tooltip.jsx'
import { getExtensionResults, subscribeExtensionResults } from '../host/registry'

export default function ExtensionBadge({ itemId, tooltipsEnabled }) {
  const { t } = useTranslation()
  // 只订阅当前条目的识别结果，避免任意条目结果更新都触发全部 chip 重渲染。
  const subscribe = useCallback(
    (listener) => subscribeExtensionResults(itemId, listener),
    [itemId]
  )
  const getSnapshot = useCallback(() => getExtensionResults(itemId), [itemId])
  const chips = useSyncExternalStore(subscribe, getSnapshot)

  if (!chips || chips.length === 0) {
    return null
  }

  const visible = chips.slice(0, 2)
  const hiddenCount = chips.length - visible.length

  return (
    // `bottom-1` 在本项目 UnoCSS 产物中未生成（实测 computed bottom 无效），
    // 会让 chip 落到条目顶部并被操作条遮挡，因此这里用内联样式固定底边距。
    <div
      className="absolute right-2 z-20 flex max-w-[70%] items-center gap-1 pointer-events-none"
      style={{ bottom: 4 }}
      role={hiddenCount > 0 ? 'group' : undefined}
      aria-label={
        hiddenCount > 0
          ? t('extensions.moreResults', { total: chips.length, shown: visible.length })
          : undefined
      }
    >
      {visible.map((chip, index) => (
        <Tooltip
          key={`${chip.extensionId}-${index}`}
          content={chip.detail || chip.label}
          placement="left"
          tooltipsEnabled={tooltipsEnabled}
        >
          <span
            className="pointer-events-auto inline-flex max-w-[120px] items-center truncate rounded-full border border-qc-border bg-qc-panel px-1.5 py-[1px] text-[10px] leading-[14px] text-qc-fg-muted"
            onPointerDown={(event) => event.stopPropagation()}
            onMouseDown={(event) => event.stopPropagation()}
            onClick={(event) => {
              event.stopPropagation()
              event.preventDefault()
            }}
            onDoubleClick={(event) => event.stopPropagation()}
          >
            <i className="ti ti-puzzle mr-0.5 shrink-0" style={{ fontSize: 10 }}></i>
            <span className="truncate">{chip.label}</span>
          </span>
        </Tooltip>
      ))}
    </div>
  )
}
