import '@shared/styles/tabler-icons-subset.css';
import { useTranslation } from 'react-i18next';
import { useCallback, useEffect, useRef, useState } from 'react';
import { useSnapshot } from 'valtio';
import { settingsStore } from '@shared/store/settingsStore';
import Tooltip from '@shared/components/common/Tooltip.jsx';
import { computeAnchoredPanelStyle } from '../utils/anchorPanel';

const LISTBOX_ID = 'qc-plugin-filter-listbox';
const ALL_OPTION_ID = 'qc-plugin-filter-option-all';
const PANEL_MIN_WIDTH = 180;
const PANEL_MAX_HEIGHT = 240;
const VIEWPORT_MARGIN = 4;

/**
 * 插件筛选下拉：单选，「全部」代表不筛选。
 * 定位与翻转逻辑与 FilterTypeDropdown 共用 anchorPanel，避免两套坐标计算漂移。
 */
function PluginFilterDropdown({
  extensions = [],
  selectedId = 'all',
  onChange,
  mode = 'horizontal',
  showLabel = true,
  compact = false,
}) {
  const { t } = useTranslation();
  const settings = useSnapshot(settingsStore);
  const uiAnimationEnabled = settings.uiAnimationEnabled !== false;
  const [isOpen, setIsOpen] = useState(false);
  const [activeIndex, setActiveIndex] = useState(-1);
  const [panelStyle, setPanelStyle] = useState(null);
  const triggerRef = useRef(null);
  const panelRef = useRef(null);

  const isSidebar = mode === 'sidebar';
  const isCompact = compact && !isSidebar;
  const selectedExtension = extensions.find(extension => extension.id === selectedId);
  const isFiltering = Boolean(selectedExtension);
  const triggerLabel = isFiltering
    ? `${t('filter.extensionFilter', '插件筛选')}: ${selectedExtension.name}`
    : t('filter.allExtensions', '全部插件');

  const closePanel = useCallback((restoreFocus = false) => {
    setIsOpen(false);
    if (restoreFocus) {
      triggerRef.current?.focus();
    }
  }, []);

  const openPanel = useCallback(() => {
    const trigger = triggerRef.current;
    if (!trigger) {
      return;
    }

    const style = computeAnchoredPanelStyle({
      triggerRect: trigger.getBoundingClientRect(),
      mode,
      minWidth: PANEL_MIN_WIDTH,
      maxHeight: PANEL_MAX_HEIGHT,
      viewportMargin: VIEWPORT_MARGIN,
    });

    setPanelStyle(style);
    setActiveIndex(extensions.findIndex(extension => extension.id === selectedId));
    setIsOpen(true);
  }, [mode, extensions, selectedId]);

  const selectOption = useCallback(id => {
    // 再点已选中项等价于取消筛选；「全部」始终恢复完整列表。
    onChange(id === selectedId ? 'all' : id);
  }, [onChange, selectedId]);

  useEffect(() => {
    if (!isOpen) {
      return;
    }

    const handlePointerDown = event => {
      if (triggerRef.current?.contains(event.target) || panelRef.current?.contains(event.target)) {
        return;
      }
      setIsOpen(false);
    };

    const handleViewportChange = () => {
      setIsOpen(false);
    };

    document.addEventListener('mousedown', handlePointerDown, true);
    window.addEventListener('resize', handleViewportChange);
    window.addEventListener('scroll', handleViewportChange, true);
    const focusFrame = requestAnimationFrame(() => panelRef.current?.focus());

    return () => {
      cancelAnimationFrame(focusFrame);
      document.removeEventListener('mousedown', handlePointerDown, true);
      window.removeEventListener('resize', handleViewportChange);
      window.removeEventListener('scroll', handleViewportChange, true);
    };
  }, [isOpen]);

  const handleTriggerKeyDown = event => {
    if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      event.preventDefault();
      event.stopPropagation();
      openPanel();
    }
  };

  const handleListboxKeyDown = event => {
    const lastIndex = extensions.length - 1;

    if (event.key === 'Escape') {
      event.preventDefault();
      event.stopPropagation();
      closePanel(true);
      return;
    }

    if (event.key === 'Tab') {
      setIsOpen(false);
      return;
    }

    if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      event.preventDefault();
      event.stopPropagation();
      setActiveIndex(prev => {
        if (event.key === 'ArrowDown') {
          return prev >= lastIndex ? -1 : prev + 1;
        }
        return prev <= -1 ? lastIndex : prev - 1;
      });
      return;
    }

    if (event.key === ' ' || event.key === 'Enter') {
      event.preventDefault();
      event.stopPropagation();
      if (activeIndex === -1) {
        onChange('all');
        return;
      }
      const extension = extensions[activeIndex];
      if (extension) {
        selectOption(extension.id);
      }
    }
  };

  const activeDescendant = activeIndex === -1
    ? ALL_OPTION_ID
    : extensions[activeIndex]
      ? `qc-plugin-filter-option-${extensions[activeIndex].id}`
      : undefined;

  const triggerClassName = isSidebar
    ? `relative z-10 flex items-center h-9 rounded-lg focus:outline-none transition-all duration-200 ${
        showLabel
          ? 'justify-start gap-0 px-3 w-full min-w-0'
          : 'justify-center gap-0 px-0 w-10'
      } ${
        isFiltering
          ? 'bg-blue-500 text-white shadow-md hover:bg-blue-500'
          : 'text-[var(--qc-fg-muted)] hover:bg-qc-hover'
      }`
    : `relative z-10 flex shrink-0 items-center justify-center ${
        isCompact ? 'w-4 h-4' : 'w-7 h-7'
      } rounded-md focus:outline-none ${
        uiAnimationEnabled ? 'active:scale-95 hover:scale-105' : ''
      } ${
        isFiltering
          ? 'text-[var(--qc-accent)]'
          : 'text-[var(--qc-fg-muted)] hover:bg-qc-hover'
      }`;

  const triggerButton = <button
    ref={triggerRef}
    type="button"
    onClick={() => (isOpen ? closePanel() : openPanel())}
    onKeyDown={handleTriggerKeyDown}
    aria-haspopup="listbox"
    aria-expanded={isOpen}
    aria-controls={isOpen ? LISTBOX_ID : undefined}
    aria-label={triggerLabel}
    className={triggerClassName}
    style={!isSidebar && uiAnimationEnabled ? {
      transitionProperty: 'transform, background-color, color',
      transitionDuration: '200ms, 500ms, 500ms'
    } : {}}
  >
    <span className="relative inline-flex items-center justify-center">
      <i className="ti ti-puzzle" style={{ fontSize: 16 }} />
    </span>
    {!isSidebar && isFiltering && (
      <span
        aria-hidden="true"
        className="pointer-events-none absolute bottom-0 left-0.5 right-0.5 h-0.5 rounded-full"
        style={{ backgroundColor: 'var(--qc-accent)' }}
      />
    )}
  </button>;

  return <>
    <Tooltip content={triggerLabel} placement={isSidebar ? 'right' : 'bottom'} disabled={isOpen} asChild>{triggerButton}</Tooltip>

    {isOpen && panelStyle && (
      <div
        id={LISTBOX_ID}
        ref={panelRef}
        role="listbox"
        aria-label={t('filter.extensionFilter', '插件筛选')}
        aria-activedescendant={activeDescendant}
        tabIndex={-1}
        onKeyDown={handleListboxKeyDown}
        className={`fixed z-[70] max-h-[240px] overflow-y-auto rounded-xl border border-qc-border bg-qc-panel py-1 shadow-2xl focus:outline-none ${
          uiAnimationEnabled ? (panelStyle.bottom ? 'animate-dropdown-up' : 'animate-dropdown-down') : ''
        }`}
        style={panelStyle}
      >
        <div
          role="option"
          id={ALL_OPTION_ID}
          aria-selected={!isFiltering}
          onMouseDown={event => event.preventDefault()}
          onMouseEnter={() => setActiveIndex(-1)}
          onClick={() => onChange('all')}
          className={`mx-1 flex h-8 cursor-pointer select-none items-center gap-2 rounded-lg px-2 text-[12px] ${
            activeIndex === -1 ? 'bg-qc-hover' : ''
          } ${!isFiltering ? 'text-[var(--qc-accent)]' : 'text-qc-fg'}`}
        >
          <i className="ti ti-layers-subtract" style={{ fontSize: 14 }} />
          <span className="flex-1 truncate">{t('filter.allExtensions', '全部插件')}</span>
          {!isFiltering && <i className="ti ti-check" style={{ fontSize: 12 }} />}
        </div>

        {extensions.map((extension, index) => {
          const selected = selectedExtension?.id === extension.id;
          const active = activeIndex === index;
          return <div
            key={extension.id}
            role="option"
            id={`qc-plugin-filter-option-${extension.id}`}
            aria-selected={selected}
            onMouseDown={event => event.preventDefault()}
            onMouseEnter={() => setActiveIndex(index)}
            onClick={() => selectOption(extension.id)}
            className={`mx-1 flex cursor-pointer select-none items-center gap-2 rounded-lg px-2 py-1.5 text-[12px] ${
              active ? 'bg-qc-hover' : ''
            } ${selected ? 'text-[var(--qc-accent)]' : 'text-qc-fg'}`}
          >
            <i className="ti ti-puzzle" style={{ fontSize: 14 }} />
            <span className="flex min-w-0 flex-1 flex-col leading-4">
              <span className="truncate">{extension.name}</span>
              {extension.detail && (
                <span className="truncate text-[11px] text-[var(--qc-fg-muted)]">{extension.detail}</span>
              )}
            </span>
            {selected && <i className="ti ti-check" style={{ fontSize: 12 }} />}
          </div>;
        })}
      </div>
    )}
  </>;
}

export default PluginFilterDropdown;
