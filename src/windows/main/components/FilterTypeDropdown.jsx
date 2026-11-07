import '@shared/styles/tabler-icons-subset.css';
import { useTranslation } from 'react-i18next';
import { useCallback, useEffect, useRef, useState } from 'react';
import { useSnapshot } from 'valtio';
import { settingsStore } from '@shared/store/settingsStore';
import Tooltip from '@shared/components/common/Tooltip.jsx';

const LISTBOX_ID = 'qc-filter-type-listbox';
const ALL_OPTION_ID = 'qc-filter-type-option-all';
const PANEL_MIN_WIDTH = 136;
const PANEL_MAX_HEIGHT = 240;
const VIEWPORT_MARGIN = 4;

function FilterTypeDropdown({
  filters,
  selectedIds = [],
  onChange,
  mode = 'horizontal',
  showLabel = true,
  compact = false
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
  const selectedTypes = selectedIds
    .map(id => filters.find(filter => filter.id === id))
    .filter(Boolean);
  const selectedCount = selectedTypes.length;
  const triggerIcon = selectedCount === 1 ? selectedTypes[0].icon : 'ti ti-category';
  const triggerLabel = selectedCount === 0
    ? t('filter.allTypes', '全部类型')
    : selectedCount === 1
      ? `${t('filter.typeFilter', '类型筛选')}: ${selectedTypes[0].label}`
      : `${t('filter.typeFilter', '类型筛选')}: ${selectedTypes.map(filter => filter.label).join(', ')}, ${t('filter.count', { count: selectedCount })}`;

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

    const rect = trigger.getBoundingClientRect();
    const width = Math.min(
      Math.max(rect.width, PANEL_MIN_WIDTH),
      Math.max(PANEL_MIN_WIDTH, window.innerWidth - VIEWPORT_MARGIN * 2)
    );
    let left = mode === 'sidebar' ? rect.right + VIEWPORT_MARGIN : rect.left;
    if (left + width > window.innerWidth - VIEWPORT_MARGIN) {
      left = Math.max(VIEWPORT_MARGIN, window.innerWidth - width - VIEWPORT_MARGIN);
    }

    // 依据真实可用空间决定展开方向：下方不足且上方更充裕时向上展开，
    // maxHeight 按可用空间钳制，两边都不够时由面板自身滚动兜底。
    const spaceBelow = window.innerHeight - rect.bottom - VIEWPORT_MARGIN;
    const spaceAbove = rect.top - VIEWPORT_MARGIN;
    const placeAbove = spaceBelow < PANEL_MAX_HEIGHT && spaceAbove > spaceBelow;
    // 以选定方向的实际可用空间钳制 maxHeight，两边都不足时由面板内部滚动兜底。
    const availableHeight = Math.max(
      0,
      Math.min(PANEL_MAX_HEIGHT, (placeAbove ? spaceAbove : spaceBelow) - VIEWPORT_MARGIN)
    );

    const style = {
      left: `${Math.round(left)}px`,
      width: `${Math.round(width)}px`,
      maxHeight: `${Math.round(availableHeight)}px`
    };
    if (placeAbove) {
      style.bottom = `${Math.round(window.innerHeight - rect.top + VIEWPORT_MARGIN)}px`;
    } else {
      style.top = `${Math.round(rect.bottom + VIEWPORT_MARGIN)}px`;
    }

    setPanelStyle(style);
    setActiveIndex(-1);
    setIsOpen(true);
  }, [mode]);

  const toggleFilter = useCallback(id => {
    const nextIds = selectedIds.includes(id)
      ? selectedIds.filter(filterId => filterId !== id)
      : [...selectedIds, id];
    onChange(nextIds);
  }, [onChange, selectedIds]);

  useEffect(() => {
    if (!isOpen) {
      return undefined;
    }

    const focusFrame = requestAnimationFrame(() => {
      panelRef.current?.focus();
    });

    const handlePointerDown = event => {
      if (panelRef.current?.contains(event.target) || triggerRef.current?.contains(event.target)) {
        return;
      }
      setIsOpen(false);
    };
    const handleViewportChange = () => setIsOpen(false);

    document.addEventListener('mousedown', handlePointerDown, true);
    window.addEventListener('resize', handleViewportChange);
    window.addEventListener('scroll', handleViewportChange, true);

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
    const lastIndex = filters.length - 1;

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
        onChange([]);
        return;
      }
      const filter = filters[activeIndex];
      if (filter) {
        toggleFilter(filter.id);
      }
    }
  };

  const activeDescendant = activeIndex === -1
    ? ALL_OPTION_ID
    : activeIndex >= 0 && filters[activeIndex]
      ? `qc-filter-type-option-${filters[activeIndex].id}`
      : undefined;

  const triggerClassName = isSidebar
    ? `relative z-10 flex items-center h-9 rounded-lg focus:outline-none transition-all duration-200 ${
        showLabel
          ? 'justify-start gap-0 px-3 w-full min-w-0'
          : 'justify-center gap-0 px-0 w-10'
      } ${
        selectedCount > 0
          ? 'text-[var(--qc-accent)] hover:bg-qc-hover'
          : 'text-[var(--qc-fg-muted)] hover:bg-qc-hover'
      }`
    : `relative z-10 flex shrink-0 items-center justify-center ${
        isCompact ? 'w-4 h-4' : 'w-7 h-7'
      } rounded-md focus:outline-none ${
        uiAnimationEnabled ? 'active:scale-95 hover:scale-105' : ''
      } ${
        selectedCount > 0
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
      <i className={triggerIcon} style={{ fontSize: 16 }} />
      {selectedCount >= 2 && (
        <span
          aria-hidden="true"
          className="pointer-events-none absolute rounded-full"
          style={{
            width: 4,
            height: 4,
            right: -2,
            bottom: -1,
            backgroundColor: 'var(--qc-accent)'
          }}
        />
      )}
    </span>
    {!isSidebar && selectedCount > 0 && (
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
        aria-multiselectable="true"
        aria-label={t('filter.typeFilter', '类型筛选')}
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
          aria-selected={selectedCount === 0}
          onMouseDown={event => event.preventDefault()}
          onMouseEnter={() => setActiveIndex(-1)}
          onClick={() => onChange([])}
          className={`mx-1 flex h-8 cursor-pointer select-none items-center gap-2 rounded-lg px-2 text-[12px] ${
            activeIndex === -1 ? 'bg-qc-hover' : ''
          } ${selectedCount === 0 ? 'text-[var(--qc-accent)]' : 'text-qc-fg'}`}
        >
          <i className="ti ti-layers-subtract" style={{ fontSize: 14 }} />
          <span className="flex-1 truncate">{t('filter.allTypes', '全部类型')}</span>
          {selectedCount === 0 && <i className="ti ti-check" style={{ fontSize: 12 }} />}
        </div>

        {filters.map((filter, index) => {
          const selected = selectedIds.includes(filter.id);
          const active = activeIndex === index;
          return <div
            key={filter.id}
            role="option"
            id={`qc-filter-type-option-${filter.id}`}
            aria-selected={selected}
            onMouseDown={event => event.preventDefault()}
            onMouseEnter={() => setActiveIndex(index)}
            onClick={() => toggleFilter(filter.id)}
            className={`mx-1 flex h-8 cursor-pointer select-none items-center gap-2 rounded-lg px-2 text-[12px] ${
              active ? 'bg-qc-hover' : ''
            } ${selected ? 'text-[var(--qc-accent)]' : 'text-qc-fg'}`}
          >
            <span
              aria-hidden="true"
              className={`flex h-3.5 w-3.5 shrink-0 items-center justify-center rounded border ${
                selected ? 'border-[var(--qc-accent)] bg-[var(--qc-accent)] text-[var(--qc-accent-fg)]' : 'border-qc-border'
              }`}
            >
              {selected && <i className="ti ti-check" style={{ fontSize: 10 }} />}
            </span>
            <i className={filter.icon} style={{ fontSize: 14 }} />
            <span className="flex-1 truncate">{filter.label}</span>
          </div>;
        })}
      </div>
    )}
  </>;
}

export default FilterTypeDropdown;
