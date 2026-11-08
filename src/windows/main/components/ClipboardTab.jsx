import { useRef, forwardRef, useImperativeHandle, useEffect, useState, useCallback } from 'react';
import { useSnapshot } from 'valtio';
import { listen } from '@tauri-apps/api/event';
import { clipboardStore, refreshClipboardHistory } from '@shared/store/clipboardStore';
import { settingsStore } from '@shared/store/settingsStore';
import { navigationStore } from '@shared/store/navigationStore';
import ClipboardList from './ClipboardList';
import FloatingToolbar from './FloatingToolbar';

const SEARCH_DEBOUNCE_DELAY = 200;
const ClipboardTab = forwardRef(({
  contentFilter,
  pasteFilter = 'all',
  extensionFilter = 'all',
  searchQuery,
  onClearFilters
}, ref) => {
  const snap = useSnapshot(clipboardStore);
  const settings = useSnapshot(settingsStore);
  const listRef = useRef(null);
  const [isAtTop, setIsAtTop] = useState(true);
  const prevTotalCountRef = useRef(snap.totalCount);
  const searchDebounceRef = useRef(null);
  const scrollTopTimeoutRef = useRef(null);

  const clearScrollToTopTimeout = useCallback(() => {
    if (scrollTopTimeoutRef.current) {
      clearTimeout(scrollTopTimeoutRef.current);
      scrollTopTimeoutRef.current = null;
    }
  }, []);

  const debouncedSearch = useCallback((query, filter) => {
    if (searchDebounceRef.current) {
      clearTimeout(searchDebounceRef.current);
    }

    searchDebounceRef.current = setTimeout(async () => {
      clipboardStore.setFilter(query);
      try {
        await refreshClipboardHistory();
      } catch (error) {
        console.error('刷新剪贴板历史失败:', error);
      }
    }, query ? SEARCH_DEBOUNCE_DELAY : 0);
  }, []);

  useEffect(() => () => clearScrollToTopTimeout(), [clearScrollToTopTimeout]);

  useEffect(() => {
    clipboardStore.setContentType(contentFilter);
    clipboardStore.setPasteStatus(pasteFilter);
    debouncedSearch(searchQuery, contentFilter);

    return () => {
      if (searchDebounceRef.current) {
        clearTimeout(searchDebounceRef.current);
      }
    };
  }, [searchQuery, contentFilter, pasteFilter, debouncedSearch]);


  useEffect(() => {
    if (snap.totalCount > prevTotalCountRef.current) {
      handleScrollToTop({ checkSetting: true, delay: 100 });
    }
    prevTotalCountRef.current = snap.totalCount;
  }, [snap.totalCount]);
  useEffect(() => {
    const setupListeners = async () => {
      const unlisten1 = await listen('window-show-animation', () => handleScrollToTop({ checkSetting: true, delay: 50 }));
      const unlisten2 = await listen('edge-snap-show', () => handleScrollToTop({ checkSetting: true, delay: 50 }));
      const unlisten3 = await listen('window-hide-animation', clearScrollToTopTimeout);
      const unlisten4 = await listen('edge-snap-hide', clearScrollToTopTimeout);
      return () => {
        unlisten1();
        unlisten2();
        unlisten3();
        unlisten4();
      };
    };
    let cleanup = setupListeners();
    return () => cleanup.then(fn => fn());
  }, [settings.autoScrollToTopOnShow, clearScrollToTopTimeout]);

  // 暴露导航方法给父组件
  useImperativeHandle(ref, () => ({
    navigateUp: () => listRef.current?.navigateUp?.(),
    navigateDown: () => listRef.current?.navigateDown?.(),
    executeCurrentItem: () => listRef.current?.executeCurrentItem?.(),
    executePlainTextPaste: () => listRef.current?.executePlainTextPaste?.()
  }));

  // 处理滚动状态变化
  const handleScrollStateChange = ({
    atTop
  }) => {
    setIsAtTop(atTop);
  };

  const handleScrollToTop = (options = {}) => {
    const {
      checkSetting = true,
      delay = 0
    } = options;

    if (checkSetting && !settings.autoScrollToTopOnShow) {
      return;
    }

    clearScrollToTopTimeout();
    scrollTopTimeoutRef.current = setTimeout(() => {
      listRef.current?.scrollToTop?.();
      navigationStore.resetNavigation();
      scrollTopTimeoutRef.current = null;
    }, delay);
  };
  
  return <div className="h-full flex flex-col relative">
    {/* 列表 */}
    <ClipboardList ref={listRef} onScrollStateChange={handleScrollStateChange} onClearFilters={onClearFilters} extensionFilter={extensionFilter} />

    {/* 悬浮工具栏 */}
    <FloatingToolbar showScrollTop={!isAtTop && snap.totalCount > 0} showAddFavorite={false} onScrollTop={() => handleScrollToTop({
      checkSetting: false
    })} />
  </div>;
});
ClipboardTab.displayName = 'ClipboardTab';
export default ClipboardTab;
