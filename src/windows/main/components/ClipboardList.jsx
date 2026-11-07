import { Virtuoso } from 'react-virtuoso';
import { useCallback, useState, useMemo, useRef, forwardRef, useImperativeHandle, useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { useSnapshot } from 'valtio';
import { listen } from '@tauri-apps/api/event';
import { useCustomScrollbar } from '@shared/hooks/useCustomScrollbar';
import { useSortableList } from '@shared/hooks/useSortable';
import { useNavigation } from '@shared/hooks/useNavigation';
import { ROW_HEIGHT_CONFIG } from '@shared/hooks/useItemCommon';
import { clipboardStore, loadClipboardRange, pasteClipboardItem, refreshClipboardHistory, cancelClipboardMaintenanceRetry } from '@shared/store/clipboardStore';
import { navigationStore } from '@shared/store/navigationStore';
import { settingsStore } from '@shared/store/settingsStore';
import { getClipboardHistory, moveClipboardItemToTop, moveClipboardItemById, closePreviewWindow } from '@shared/api';
import { getOneTimePasteEnabled } from '@shared/services/oneTimePaste';
import ClipboardItem from './ClipboardItem';
import { useExternalDragSwitch } from '@shared/hooks/useExternalDragSwitch';
import ExternalDragSafeZones from '@shared/components/ExternalDragSafeZones';
import {
  extensionsStore,
  initExtensions,
  identifyItems,
  syncExtensionsEnabled,
} from '@/extensions/host/registry';
import { LIMITS } from '@/extensions/protocol';

const SCROLL_DEBOUNCE_DELAY = 50;
const LIST_PRELOAD_PADDING = 20;
const LIST_VIEWPORT_PADDING = 120;
const SELECTION_LOAD_PAGE_SIZE = 100;

const ClipboardList = forwardRef(({
  onScrollStateChange,
  onClearFilters
}, ref) => {
  const { t } = useTranslation();
  const [scrollerElement, setScrollerElement] = useState(null);
  const virtuosoRef = useRef(null);
  const currentRangeRef = useRef({
    startIndex: 0,
    endIndex: 0
  });
  const isWindowHiddenRef = useRef(false);
  const [visibleRange, setVisibleRange] = useState({ startIndex: 0, endIndex: 0 });
  const loadTimeoutRef = useRef(null);
  const loadMissingRangeRef = useRef(null);
  const selectionRequestRef = useRef(0);
  const onScrollStateChangeRef = useRef(onScrollStateChange);
  const snap = useSnapshot(navigationStore);
  const clipSnap = useSnapshot(clipboardStore);
  const isMultiSelectMode = clipSnap.isMultiSelectMode;
  const settings = useSnapshot(settingsStore);
  const showShortcut = settings.showListShortcuts !== false && !clipSnap.filter && clipSnap.contentType === 'all';
  const showIndex = settings.showListIndex !== false;
  const selectedIdSet = useMemo(() => new Set(clipSnap.selectedEntries.map(entry => entry.id)), [clipSnap.selectedEntries]);
  const selectedOrderMap = useMemo(() => new Map(clipSnap.selectedEntries.map((entry, order) => [entry.id, order + 1])), [clipSnap.selectedEntries]);
  useCustomScrollbar(scrollerElement);
  const scrollerRefCallback = useCallback(element => element && setScrollerElement(element), []);

  // 只构建“已加载”的条目（缓存上限约 120 条），不再按 totalCount 生成万级数组。
  // 未加载索引由 itemContent 直接渲染骨架，排序上下文也只注册这些已加载项。
  // 列表随滚动频繁渲染，派生数组按 items / totalCount 缓存，避免每次渲染全量重算。
  const loadedEntries = useMemo(() => {
    const entries = []
    for (const key of Object.keys(clipSnap.items)) {
      const index = Number(key)
      const item = clipSnap.items[key]
      if (!item?.id) continue
      entries.push({
        item,
        id: item.id,
        is_pinned: item.is_pinned,
        _sortId: `${item.created_at}-${index}`,
        _index: index,
      })
    }
    entries.sort((a, b) => a._index - b._index)
    return entries
  }, [clipSnap.items])

  // useNavigation 需要按绝对索引取项，这里用稀疏数组承载，length 仍等于 totalCount。
  const navigationItems = useMemo(() => {
    const items = []
    items.length = clipSnap.totalCount
    for (const entry of loadedEntries) {
      items[entry._index] = entry
    }
    return items
  }, [loadedEntries, clipSnap.totalCount])

  useEffect(() => {
    onScrollStateChangeRef.current = onScrollStateChange;
  }, [onScrollStateChange]);

  const handleDragEnd = async (oldIndex, newIndex) => {
    if (oldIndex === newIndex) return;
    const fromEntry = loadedEntries[oldIndex];
    const toEntry = loadedEntries[newIndex];

    if (!fromEntry || !toEntry || fromEntry.is_pinned !== toEntry.is_pinned) {
      return;
    }

    try {
      if (fromEntry.id && toEntry.id) {
        await moveClipboardItemById(fromEntry.id, toEntry.id);
        clipboardStore.moveLoadedItem(fromEntry._index, toEntry._index);
      }
    } catch (error) {
      console.error('移动剪贴板项失败:', error);
    }
  };

  const {
    DndContext,
    SortableContext,
    DragOverlay,
    sensors,
    handleDragStart,
    handleDragEnd: onDragEnd,
    handleDragCancel,
    activeId,
    activeItem,
    strategy,
    modifiers,
    collisionDetection
  } = useSortableList({
    items: loadedEntries,
    onDragEnd: handleDragEnd,
    restrictToVertical: false,
  });

  const {
    showSafeZones,
    prepareExternalDrag,
    shouldCancelDndDrop,
    handleDndDragStart,
    handleDndDragEnd,
    handleDndDragCancel,
  } = useExternalDragSwitch({
    onDragStart: handleDragStart,
    onDragEnd: onDragEnd,
    onDragCancel: handleDragCancel,
    closePreview: () => closePreviewWindow().catch(() => { }),
  });

  const activeIndex = activeItem ? loadedEntries.findIndex(item => item._sortId === activeId || item.id === activeId) : -1;

  useEffect(() => {
    if (!activeId || !scrollerElement) return;

    const handleWheel = (e) => {
      e.preventDefault();
      scrollerElement.scrollTop += e.deltaY;
    };

    document.addEventListener('wheel', handleWheel, { passive: false });
    return () => document.removeEventListener('wheel', handleWheel);
  }, [activeId, scrollerElement]);

  useEffect(() => {
    if (!activeId) return;
    document.body.classList.add('dragging-cursor');
    return () => {
      document.body.classList.remove('dragging-cursor');
    };
  }, [activeId]);

  const {
    currentSelectedIndex,
    navigateUp,
    navigateDown,
    executeCurrentItem,
    handleItemHover,
    handleScrollStart,
    handleScrollEnd
  } = useNavigation({
    items: navigationItems,
    virtuosoRef,
    onExecuteItem: async (item, index) => {
      // 未加载的占位项没有 id：先触发该索引的加载，本次不执行粘贴。
      if (!item?.id) {
        if (Number.isInteger(index) && index >= 0) {
          loadMissingRangeRef.current?.(index, index).catch(error => {
            console.error('加载待执行剪贴板项失败:', error);
          });
        }
        return;
      }
      try {
        await pasteClipboardItem(item.id);
        // 粘贴后置顶：本地移动已加载项，避免清空 items 造成整表骨架屏。
        if (!getOneTimePasteEnabled() && settingsStore.pasteToTop && item.id && !item.is_pinned) {
          try {
            await moveClipboardItemToTop(item.id);
          } finally {
            clipboardStore.moveLoadedItemToTop(item.id);
          }
        }
      } catch (error) {
        console.error('粘贴失败:', error);
      }
    },
    enabled: snap.activeTab === 'clipboard' && !isMultiSelectMode
  });

  const handleVirtuosoScrollState = useCallback((scrolling) => {
    if (scrolling) {
      // 滚动时主动关闭预览，避免鼠标静止滚动导致旧预览残留
      closePreviewWindow().catch(() => { });
      handleScrollStart();
      return;
    }
    handleScrollEnd();
  }, [handleScrollEnd, handleScrollStart]);

  useEffect(() => {
    currentRangeRef.current = {
      startIndex: 0,
      endIndex: 0
    };
    clipboardStore.currentViewRange = { start: 0, end: 50 };

    if (loadTimeoutRef.current) {
      clearTimeout(loadTimeoutRef.current);
      loadTimeoutRef.current = null;
    }

    virtuosoRef.current?.scrollToIndex({
      index: 0,
      align: 'start',
      behavior: 'auto'
    });
    if (scrollerElement) {
      scrollerElement.scrollTop = 0;
    }
    navigationStore.resetNavigation();
    onScrollStateChangeRef.current?.({ atTop: true });
  }, [clipSnap.filter, clipSnap.contentType, clipSnap.pasteStatus, scrollerElement]);

  const loadMissingRange = useCallback(async (startIndex, endIndex) => {
    if (clipSnap.loading || clipSnap.totalCount <= 0) {
      return false;
    }

    let safeStart = Number.isInteger(startIndex) ? startIndex : 0;
    let safeEnd = Number.isInteger(endIndex) ? endIndex : Math.min(49, clipSnap.totalCount - 1);

    if (safeEnd < safeStart) {
      safeStart = 0;
      safeEnd = Math.min(49, clipSnap.totalCount - 1);
    }

    safeStart = Math.max(0, Math.min(safeStart, clipSnap.totalCount - 1));
    safeEnd = Math.max(safeStart, Math.min(safeEnd, clipSnap.totalCount - 1));

    let rangeStart = -1;
    let rangeEnd = -1;
    for (let index = safeStart; index <= safeEnd; index += 1) {
      if (!clipboardStore.hasItem(index)) {
        if (rangeStart === -1) rangeStart = index;
        rangeEnd = index;
      }
    }

    if (rangeStart === -1) {
      return false;
    }

    const loadStart = Math.max(0, rangeStart - LIST_PRELOAD_PADDING);
    const loadEnd = Math.min(clipSnap.totalCount - 1, rangeEnd + LIST_PRELOAD_PADDING);
    await loadClipboardRange(loadStart, loadEnd);
    return true;
  }, [clipSnap.loading, clipSnap.totalCount]);

  useEffect(() => {
    loadMissingRangeRef.current = loadMissingRange;
  }, [loadMissingRange]);

  useEffect(() => {
    const handleWindowHide = () => {
      isWindowHiddenRef.current = true;
      if (loadTimeoutRef.current) {
        clearTimeout(loadTimeoutRef.current);
        loadTimeoutRef.current = null;
      }
    };

    const handleWindowShow = () => {
      isWindowHiddenRef.current = false;
      // 隐藏期间 clipboard-updated 只累计了计数，显示后需要一次完整刷新来校正索引。
      if (clipboardStore.pendingRefresh) {
        setTimeout(() => {
          clipboardStore.pendingRefresh = false;
          refreshClipboardHistory().catch(error => {
            console.error('窗口显示后刷新剪贴板失败:', error);
          });
        }, 80);
        return;
      }
      setTimeout(() => {
        const { startIndex, endIndex } = currentRangeRef.current;
        loadMissingRangeRef.current?.(startIndex, endIndex).catch(error => {
          console.error('窗口显示后补齐剪贴板范围失败:', error);
        });
      }, 80);
    };

    const setupListeners = async () => {
      const unlisten1 = await listen('window-hide-animation', handleWindowHide);
      const unlisten2 = await listen('edge-snap-hide', handleWindowHide);
      const unlisten3 = await listen('window-show-animation', handleWindowShow);
      const unlisten4 = await listen('edge-snap-show', handleWindowShow);
      return () => {
        unlisten1();
        unlisten2();
        unlisten3();
        unlisten4();
      };
    };

    const cleanup = setupListeners();
    return () => {
      // 卸载后不再补刷，避免维护重试定时器在窗口销毁后继续请求。
      cancelClipboardMaintenanceRetry();
      cleanup.then(fn => fn());
    };
  }, []);

  const loadSelectionEntries = useCallback(async (startIndex, endIndex) => {
    const entries = [];

    for (let offset = startIndex; offset <= endIndex; offset += SELECTION_LOAD_PAGE_SIZE) {
      const limit = Math.min(SELECTION_LOAD_PAGE_SIZE, endIndex - offset + 1);
      const result = await getClipboardHistory({
        offset,
        limit,
        contentType: clipSnap.contentType !== 'all' ? clipSnap.contentType : undefined,
        search: clipSnap.filter || undefined,
      });

      result.items.forEach((item, itemOffset) => {
        if (!item?.id) return;
        entries.push({
          id: item.id,
          index: offset + itemOffset,
          contentType: item.content_type,
        });
      });

      if (result.items.length < limit) {
        break;
      }
    }

    return entries;
  }, [clipSnap.contentType, clipSnap.filter]);

  const handleItemClick = useCallback(async (item, index, event) => {
    if (settingsStore.modifierClickMultiSelect === false) {
      return false;
    }

    const isCtrlLikePressed = Boolean(event?.ctrlKey || event?.metaKey);
    const isShiftPressed = Boolean(event?.shiftKey);

    const currentlyMultiSelect = clipboardStore.isMultiSelectMode;
    if (!currentlyMultiSelect) {
      if (!isCtrlLikePressed && !isShiftPressed) {
        return false;
      }

      clipboardStore.enterMultiSelectMode({
        id: item.id,
        index,
        contentType: item.content_type,
      });

      if (isShiftPressed) {
        const currentSelectedIndex = navigationStore.currentSelectedIndex;
        const anchorIndex = typeof currentSelectedIndex === 'number' && currentSelectedIndex >= 0
          ? currentSelectedIndex
          : index;
        const startIndex = Math.min(anchorIndex, index);
        const endIndex = Math.max(anchorIndex, index);
        // 先同步记录锚点，避免范围加载期间的下一次 Shift 点击被当成普通追加选择。
        clipboardStore.setSelectionAnchorIndex(anchorIndex);
        const requestId = ++selectionRequestRef.current;
        const entries = await loadSelectionEntries(startIndex, endIndex);
        if (requestId === selectionRequestRef.current) {
          clipboardStore.selectRange(anchorIndex > index ? entries.reverse() : entries);
        }
        return true;
      }

      clipboardStore.setSelectionAnchorIndex(index);
      return true;
    }

    const selectionAnchorIndex = clipboardStore.selectionAnchorIndex;
    if (isShiftPressed && typeof selectionAnchorIndex === 'number') {
      const startIndex = Math.min(selectionAnchorIndex, index);
      const endIndex = Math.max(selectionAnchorIndex, index);
      const requestId = ++selectionRequestRef.current;
      const entries = await loadSelectionEntries(startIndex, endIndex);
      if (requestId === selectionRequestRef.current) {
        clipboardStore.selectRange(selectionAnchorIndex > index ? entries.reverse() : entries);
      }
      return true;
    }

    selectionRequestRef.current += 1;

    const entry = {
      id: item.id,
      index,
      contentType: item.content_type,
    };
    if (isCtrlLikePressed) {
      clipboardStore.toggleSelectedEntry(entry);
    } else if (clipboardStore.selectedEntries.length === 1 && clipboardStore.hasSelectedId(entry.id)) {
      clipboardStore.toggleSelectedEntry(entry);
    } else {
      // 多选模式下普通单击改为单选当前项，符合常见文件管理器行为。
      clipboardStore.replaceSelection([entry]);
    }
    clipboardStore.setSelectionAnchorIndex(index);
    return true;
  }, [loadSelectionEntries]);

  const handleRangeChanged = useCallback(({
    startIndex,
    endIndex
  }) => {
    if (isWindowHiddenRef.current || (scrollerElement && scrollerElement.clientHeight <= 1)) {
      return;
    }

    currentRangeRef.current = {
      startIndex,
      endIndex
    };

    if (loadTimeoutRef.current) {
      clearTimeout(loadTimeoutRef.current);
    }

    loadTimeoutRef.current = setTimeout(() => {
      clipboardStore.updateViewRange(startIndex, endIndex);
      setVisibleRange({ startIndex, endIndex });

      loadMissingRange(startIndex, endIndex).catch(error => {
        console.error('加载当前剪贴板范围失败:', error);
      });
    }, SCROLL_DEBOUNCE_DELAY);
  }, [loadMissingRange, scrollerElement]);

  const itemsCount = Object.keys(clipSnap.items).length;

  useEffect(() => {
    if (clipSnap.loading || clipSnap.totalCount <= 0) {
      return;
    }

    const { startIndex, endIndex } = currentRangeRef.current;
    const shouldLoadInitialPage = itemsCount === 0 && startIndex === 0 && endIndex === 0;
    loadMissingRange(
      startIndex,
      shouldLoadInitialPage ? Math.min(49, clipSnap.totalCount - 1) : endIndex,
    ).catch(error => {
      console.error('补齐剪贴板可见范围失败:', error);
    });
  }, [clipSnap.loading, clipSnap.totalCount, itemsCount, loadMissingRange]);

  const extensionSnap = useSnapshot(extensionsStore);

  useEffect(() => {
    if (settings.extensionsEnabled === false) {
      syncExtensionsEnabled(false);
    } else {
      syncExtensionsEnabled(true);
      initExtensions();
    }
  }, [settings.extensionsEnabled]);

  // 对当前可见窗口内的条目做渐进识别。
  // 批大小按实际可见条数（含上下预渲染）计算，避免大窗口/紧凑行高时第 21 条以后没有 chip。
  useEffect(() => {
    if (!extensionSnap.enabled || !extensionSnap.loaded) {
      return;
    }

    const collected = [];
    const visibleCount = visibleRange.endIndex - visibleRange.startIndex + 1;
    const maxBatch = Math.min(LIMITS.maxBatchItems, Math.max(20, visibleCount));
    const endIndex = Math.max(visibleRange.endIndex, visibleRange.startIndex + maxBatch - 1);
    for (
      let index = visibleRange.startIndex;
      index <= endIndex && collected.length < maxBatch;
      index += 1
    ) {
      const item = clipboardStore.items[index];
      if (item?.id) collected.push(item);
    }

    if (collected.length === 0) {
      const indexes = Object.keys(clipboardStore.items)
        .map(Number)
        .sort((a, b) => a - b)
        .slice(0, maxBatch);
      for (const index of indexes) {
        const item = clipboardStore.items[index];
        if (item?.id) collected.push(item);
      }
    }

    if (collected.length > 0) {
      identifyItems(collected).catch(error => {
        console.warn('扩展识别失败:', error);
      });
    }
  }, [
    extensionSnap.enabled,
    extensionSnap.loaded,
    extensionSnap.revision,
    visibleRange.startIndex,
    visibleRange.endIndex,
    itemsCount,
  ]);

  useImperativeHandle(ref, () => ({
    navigateUp,
    navigateDown,
    executeCurrentItem,
    executePlainTextPaste: async () => {
      if (isMultiSelectMode) {
        return;
      }
      const entry = navigationItems[currentSelectedIndex];
      if (entry?.item && !entry._isPlaceholder) {
        try {
          const { pasteClipboardItem } = await import('@shared/api/clipboard');
          await pasteClipboardItem(entry.item.id, 'plain_text');
          if (!getOneTimePasteEnabled() && settingsStore.pasteToTop && entry.item.id && !entry.item.is_pinned) {
            try {
              await moveClipboardItemToTop(entry.item.id);
            } finally {
              clipboardStore.moveLoadedItemToTop(entry.item.id);
            }
          }
        } catch (error) {
          console.error('纯文本粘贴失败:', error);
        }
      }
    },
    scrollToTop: () => {
      virtuosoRef.current?.scrollToIndex({
        index: 0,
        align: 'start',
        behavior: 'auto'
      });
    }
  }), [currentSelectedIndex, executeCurrentItem, isMultiSelectMode, navigationItems, navigateDown, navigateUp, settings.pasteToTop]);
  if (clipSnap.loading && clipSnap.filter) {
    return <div className="flex-1 bg-qc-surface overflow-hidden flex items-center justify-center transition-colors duration-500" data-no-drag>
      <div className="flex flex-col items-center gap-2 text-[var(--qc-fg-subtle)]">
        <div className="h-5 w-5 animate-spin rounded-full border-2 border-qc-border border-t-[var(--theme-9)]"></div>
        <p className="text-sm">{t('common.searching', '搜索中...')}</p>
      </div>
    </div>;
  }

  // 首屏尚未完成第一次加载时，不要提前显示“暂无记录”，避免空态闪烁。
  if (!clipSnap.hasLoadedOnce) {
    return <div className="flex-1 bg-qc-surface overflow-hidden flex items-center justify-center transition-colors duration-500" data-no-drag>
      <div className="flex flex-col items-center gap-2 text-[var(--qc-fg-subtle)]">
        <div className="h-5 w-5 animate-spin rounded-full border-2 border-qc-border border-t-[var(--theme-9)]"></div>
        <p className="text-sm">{t('common.loading', '加载中...')}</p>
      </div>
    </div>;
  }

  if (clipSnap.totalCount === 0) {
    const hasActiveFilters = (clipSnap.contentType && clipSnap.contentType !== 'all')
      || (clipSnap.pasteStatus && clipSnap.pasteStatus !== 'all');
    return <div className="flex-1 bg-qc-surface overflow-hidden flex items-center justify-center transition-colors duration-500" data-no-drag>
      {clipSnap.filter ? (
        <p className="text-[var(--qc-fg-subtle)] text-sm">{t('common.noResults', '无搜索结果')}</p>
      ) : hasActiveFilters ? (
        <div className="flex flex-col items-center gap-2">
          <p className="text-[var(--qc-fg-subtle)] text-sm">{t('clipboard.emptyFilter', '当前筛选无记录')}</p>
          <button
            type="button"
            onClick={onClearFilters}
            aria-label={t('common.clearFilters', '清除筛选')}
            className="text-sm text-[var(--qc-accent)] hover:underline focus:outline-none py-1.5 -my-1.5"
          >
            {t('common.clearFilters', '清除筛选')}
          </button>
        </div>
      ) : (
        <p className="text-[var(--qc-fg-subtle)] text-sm">{t('clipboard.empty', '暂无剪贴板记录')}</p>
      )}
    </div>;
  }
  const rowConfig = ROW_HEIGHT_CONFIG[settings.rowHeight] || ROW_HEIGHT_CONFIG.medium;
  const isCardStyle = settings.listStyle === 'card';
  const cardSpacingPx = typeof settings.cardSpacing === 'number' ? settings.cardSpacing : 8;
  const defaultHeight = isCardStyle ? rowConfig.cardPx + cardSpacingPx : rowConfig.px;
  const heightClass = isCardStyle ? rowConfig.cardClass : rowConfig.class;
  const isBackground = settings.theme === 'background';
  const tooltipsEnabled = settings.tooltipsEnabled !== false;
  const searchKeyword = clipSnap.filter || '';
  const activeFilterType = clipSnap.contentType || 'all';
  const getCardOuterStyle = (index) => isCardStyle ? {
    paddingLeft: '0.625rem',
    paddingRight: '0.625rem',
    marginTop: index === 0 ? `${cardSpacingPx}px` : undefined,
    marginBottom: `${cardSpacingPx}px`
  } : undefined;

  return <DndContext sensors={sensors} collisionDetection={collisionDetection} onDragStart={handleDndDragStart} onDragEnd={handleDndDragEnd} onDragCancel={handleDndDragCancel} cancelDrop={shouldCancelDndDrop} modifiers={[]}>
    <ExternalDragSafeZones visible={showSafeZones} />
    <div className="flex-1 bg-qc-surface overflow-hidden custom-scrollbar-container transition-colors duration-500" data-no-drag>
      <SortableContext items={loadedEntries.map(item => item._sortId)} strategy={strategy}>
        <Virtuoso ref={virtuosoRef} totalCount={clipSnap.totalCount || 0} scrollerRef={scrollerRefCallback} atTopStateChange={atTop => {
          onScrollStateChange?.({
            atTop
          });
        }} rangeChanged={handleRangeChanged} increaseViewportBy={{
          top: LIST_VIEWPORT_PADDING,
          bottom: LIST_VIEWPORT_PADDING
        }} defaultItemHeight={defaultHeight} itemContent={index => {
          const item = clipSnap.items[index];
          if (!item?.id) {
            return isCardStyle ? <div style={getCardOuterStyle(index)}>
              <div className={heightClass}>
                <div className="h-full rounded-lg border border-qc-border bg-qc-panel p-3 animate-pulse">
                  <div className="h-4 bg-[var(--qc-panel-2)] rounded w-3/4 mb-2"></div>
                  <div className="h-3 bg-[var(--qc-panel-2)] rounded w-1/2"></div>
                </div>
              </div>
            </div> : <div className={heightClass}>
              <div className="h-full border-b border-qc-border bg-qc-panel p-3 animate-pulse">
                <div className="h-4 bg-[var(--qc-panel-2)] rounded w-3/4 mb-2"></div>
                <div className="h-3 bg-[var(--qc-panel-2)] rounded w-1/2"></div>
              </div>
            </div>;
          }

          const dragActive = Boolean(activeId);
          const animationDelay = settings.uiAnimationEnabled !== false ? Math.min(index * 20, 100) : 0;
          const sortId = `${item.created_at}-${index}`;
          return isCardStyle ? <div style={getCardOuterStyle(index)}>
            <div className={heightClass}>
              <ClipboardItem
                item={item}
                index={index}
                sortId={sortId}
                settings={settings}
                searchKeyword={searchKeyword}
                activeFilterType={activeFilterType}
                isBackground={isBackground}
                tooltipsEnabled={tooltipsEnabled}
                isSelected={!isMultiSelectMode && currentSelectedIndex === index}
                isMultiSelected={selectedIdSet.has(item.id)}
                selectionNumber={selectedOrderMap.get(item.id)}
                isMultiSelectMode={isMultiSelectMode}
                onHover={handleItemHover}
                onClick={handleItemClick}
                isDragActive={!isMultiSelectMode && dragActive}
                isDraggable={!isMultiSelectMode}
                showShortcut={showShortcut}
                showIndex={showIndex}
                animationDelay={animationDelay}
                leftClickAction={settings.leftClickAction}
                onPrepareExternalDrag={prepareExternalDrag}
              />
            </div>
          </div> : <div className={heightClass}>
            <ClipboardItem
              item={item}
              index={index}
              sortId={sortId}
              settings={settings}
              searchKeyword={searchKeyword}
              activeFilterType={activeFilterType}
              isBackground={isBackground}
              tooltipsEnabled={tooltipsEnabled}
              isSelected={!isMultiSelectMode && currentSelectedIndex === index}
              isMultiSelected={selectedIdSet.has(item.id)}
              selectionNumber={selectedOrderMap.get(item.id)}
              isMultiSelectMode={isMultiSelectMode}
              onHover={handleItemHover}
              onClick={handleItemClick}
              isDragActive={!isMultiSelectMode && dragActive}
              isDraggable={!isMultiSelectMode}
              showShortcut={showShortcut}
              showIndex={showIndex}
              animationDelay={animationDelay}
              leftClickAction={settings.leftClickAction}
              onPrepareExternalDrag={prepareExternalDrag}
            />
          </div>;
        }} isScrolling={handleVirtuosoScrollState} style={{
          height: '100%'
        }} />
      </SortableContext>
    </div>

    <DragOverlay dropAnimation={null} modifiers={[]} zIndex={1200}>
      {activeItem && activeIndex !== -1 && (() => {
      
        return (
          <div className={`${settings.rowHeight === 'auto' ? 'h-auto' : heightClass} relative z-[1200] rounded-md border border-qc-border shadow-lg bg-qc-panel/70 backdrop-blur-md`}>
            <ClipboardItem
              item={activeItem.item}
              index={activeIndex}
              sortId={activeItem._sortId}
              settings={settings}
              searchKeyword={searchKeyword}
              activeFilterType={activeFilterType}
              isBackground={isBackground}
              tooltipsEnabled={tooltipsEnabled}
              isDragActive={!isMultiSelectMode}
              isDraggable={false}
              showShortcut={showShortcut}
              showIndex={showIndex}
              leftClickAction={settings.leftClickAction}
            />
          </div>
        );
      })()}
    </DragOverlay>
  </DndContext>;
});
ClipboardList.displayName = 'ClipboardList';
export default ClipboardList;
