import { createDragPreviewIcon, createImagesDragPreviewIcon } from './dragPreviewIcon';
import { parseFilesContent } from './filesContent';
import { createLruCache } from './lruCache';

// 拖拽信息含预览图标（可能是较大 base64）与路径数组，按字节预算再做一层保护。
const externalDragInfoCache = createLruCache({
  maxEntries: 500,
  maxBytes: 8 * 1024 * 1024,
  estimateBytes: (value) => {
    const iconBytes = (value?.previewIcon?.length || 0) * 2;
    const pathBytes = (value?.paths || []).reduce((sum, path) => sum + (path?.length || 0) * 2, 0);
    return iconBytes + pathBytes;
  },
});

function buildCacheKey(item, renderType, source) {
  return `${source || ''}|${renderType}|${item?.id || ''}|${item?.content?.length || 0}`;
}

function computeDragPaths(item, renderType) {
  const filesData = parseFilesContent(item.content, item.id);
  const files = filesData?.files || [];

  if (renderType === 'image') {
    const first = files[0];
    const path = first?.exists === false ? null : first?.actual_path || first?.path;
    return path
      ? { kind: 'image', paths: [path], canDrag: true }
      : { kind: 'none', paths: [], canDrag: false };
  }

  const draggableFiles = files.filter(file => file.exists !== false && file.path);
  const paths = draggableFiles.map(file => file.path);
  if (!paths.length) {
    return { kind: 'none', paths: [], canDrag: false };
  }

  return {
    kind: 'file',
    paths,
    previewIcon: draggableFiles.find(file => file.icon_data)?.icon_data || '',
    canDrag: true,
  };
}

export function getExternalDragInfo(item, renderType, t, fallbackIcon, source) {
  const isImage = renderType === 'image';
  const isFile = renderType === 'file';
  if (!isImage && !isFile) {
    if (renderType === 'text' || renderType === 'rich_text') {
      if (!source || !item?.id) return { paths: [], item: null, iconPath: null };
      return { paths: [], item: null, textSource: { source, itemId: item.id }, iconPath: fallbackIcon, canDrag: true };
    }
    return { paths: [], item: null, iconPath: null };
  }
  if (!item.content?.startsWith('files:')) return { paths: [], item: null, iconPath: null };

  const cacheKey = buildCacheKey(item, renderType, source);
  let dragData = externalDragInfoCache.get(cacheKey);
  if (!dragData) {
    dragData = computeDragPaths(item, renderType);
    externalDragInfoCache.set(cacheKey, dragData);
  }

  if (!dragData.canDrag) {
    return { paths: [], item: null, iconPath: null };
  }

  if (dragData.kind === 'image') {
    return {
      paths: dragData.paths,
      item: dragData.paths,
      iconPath: ({ paths }) => createImagesDragPreviewIcon(paths),
      canDrag: true,
    };
  }

  return {
    paths: dragData.paths,
    item: dragData.paths,
    iconPath: ({ paths: previewPaths, mode }) => createDragPreviewIcon(dragData.previewIcon, previewPaths.length, mode, {
      copy: t('common.copy', '复制'),
      move: t('common.move', '移动'),
    }) || previewPaths[0],
    canDrag: true,
  };
}

