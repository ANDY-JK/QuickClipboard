import { useState, useEffect, useRef, useCallback } from 'react';
import { convertFileSrc } from '@tauri-apps/api/core';
import { useTranslation } from 'react-i18next';
import { useSnapshot } from 'valtio';
import { settingsStore } from '@shared/store/settingsStore';
import { formatFileSize } from '@shared/utils/format';
import { parseFilesContent } from '@shared/utils/filesContent';
import { getDataDirectory } from '@shared/utils/dataDirectory';
import { getImageThumbnail } from '@shared/api/imageThumbnail';
import Tooltip from '@shared/components/common/Tooltip.jsx';

const MAX_IMAGE_RETRIES = 4;
const RETRY_BASE_DELAY_MS = 1000;
const RETRY_MAX_DELAY_MS = 8000;
// 提前一屏挂载，避免滚动时出现明显空洞。
const VIEWPORT_ROOT_MARGIN = '300px 0px';
// 降采样时允许的最大边长（同时受显示尺寸约束），防止超大图重新编码成巨幅画布。
const MAX_DECODE_EDGE = 2048;
// 列表缩略图档位：行高最大约 280px，512 足够覆盖高 DPI 屏幕。
const THUMBNAIL_MAX_SIDE = 512;

// 图片占位配色：完整字面量常量表，避免运行时拼接
const IMAGE_PLACEHOLDER_STYLES = {
  missing: {
    gradient: 'from-red-50 to-red-50 border-red-300/60',
    icon: 'text-red-400',
    text: 'text-red-600',
    subText: 'text-red-500/70',
    badge: 'bg-red-500',
  },
  oversized: {
    gradient: 'from-amber-50 to-orange-50 border-amber-200/60',
    icon: 'text-amber-400',
    text: 'text-amber-600',
    subText: 'text-amber-500/70',
    badge: 'bg-amber-500',
  },
};

function appendRetryToken(assetUrl, retryToken) {
  return retryToken ? `${assetUrl}?retry=${retryToken}` : assetUrl;
}

function parseFirstImageId(imageId) {
  if (typeof imageId !== 'string' || !imageId.trim()) {
    return '';
  }

  return imageId
    .split(',')
    .map((part) => part.trim())
    .find((part) => part.length > 0) || '';
}

/**
 * 把 data:image 的原始数据按显示尺寸降采样后再转成 objectURL，
 * 避免列表里保留整张原图解码位图（截图类剪贴板图片常见 4K 以上）。
 * 调用方负责 URL.revokeObjectURL。
 */
async function createDownsampledImageUrl(dataUrl, targetWidth, targetHeight) {
  const response = await fetch(dataUrl);
  const blob = await response.blob();
  const bitmap = await createImageBitmap(blob);

  const maxWidth = Math.min(Math.max(targetWidth, 1), MAX_DECODE_EDGE);
  const maxHeight = Math.min(Math.max(targetHeight, 1), MAX_DECODE_EDGE);
  const scale = Math.min(1, maxWidth / bitmap.width, maxHeight / bitmap.height);
  const width = Math.max(1, Math.round(bitmap.width * scale));
  const height = Math.max(1, Math.round(bitmap.height * scale));

  if (scale >= 1) {
    bitmap.close?.();
    return URL.createObjectURL(blob);
  }

  const canvas = document.createElement('canvas');
  canvas.width = width;
  canvas.height = height;
  const context = canvas.getContext('2d');
  if (!context) {
    bitmap.close?.();
    return URL.createObjectURL(blob);
  }

  context.imageSmoothingQuality = 'high';
  context.drawImage(bitmap, 0, 0, width, height);
  bitmap.close?.();

  const outputBlob = await new Promise((resolve) => {
    // 截图类图片用 webp 能显著减小体积；不支持时回退 png。
    canvas.toBlob(resolve, 'image/webp', 0.9);
  });

  return URL.createObjectURL(outputBlob || blob);
}

function ImageContent({ item, settings: settingsProp, maxContentHeightPx }) {
  const settingsSnapshot = useSnapshot(settingsStore);
  const settings = settingsProp ?? settingsSnapshot;
  const isAutoHeight = settings.rowHeight === 'auto';
  const isXSmallHeight = settings.rowHeight === 'xsmall';
  const maxSizeMb = settings.imageMaxSizeMb || 15;
  const maxWidth = settings.imageMaxWidth || 4096;
  const maxHeight = settings.imageMaxHeight || 4096;
  const maxSizeBytes = maxSizeMb * 1024 * 1024;
  const autoMaxHeight = Number.isFinite(Number(maxContentHeightPx))
    ? Number(maxContentHeightPx)
    : 280;

  const { t } = useTranslation();

  const [imageSrc, setImageSrc] = useState(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState(false);
  const [fileExists, setFileExists] = useState(true);
  const [isOversized, setIsOversized] = useState(false);
  const [fileSize, setFileSize] = useState(null);
  const [fileName, setFileName] = useState(null);
  const [imageDimensions, setImageDimensions] = useState(null);
  const [retryToken, setRetryToken] = useState(0);
  const [isVisible, setIsVisible] = useState(false);
  const retryTimerRef = useRef(null);
  const retryCountRef = useRef(0);
  const objectUrlRef = useRef(null);
  const observerRef = useRef(null);
  const containerElementRef = useRef(null);
  const imageElementRef = useRef(null);

  const releaseObjectUrl = useCallback(() => {
    if (objectUrlRef.current) {
      URL.revokeObjectURL(objectUrlRef.current);
      objectUrlRef.current = null;
    }
  }, []);

  // 只有进入视口（提前 300px）才挂载真实图片；滚出视口时由加载 effect 卸载 src。
  const observeContainer = useCallback((node) => {
    observerRef.current?.disconnect();
    observerRef.current = null;
    containerElementRef.current = node;

    if (!node) return;
    if (typeof IntersectionObserver === 'undefined') {
      setIsVisible(true);
      return;
    }

    const observer = new IntersectionObserver(
      (entries) => {
        const entry = entries[0];
        if (entry) setIsVisible(entry.isIntersecting);
      },
      { rootMargin: VIEWPORT_ROOT_MARGIN }
    );
    observer.observe(node);
    observerRef.current = observer;
  }, []);

  useEffect(() => {
    return () => {
      observerRef.current?.disconnect();
      observerRef.current = null;
      releaseObjectUrl();
    };
  }, [releaseObjectUrl]);

  // 换条目时重置退避次数，避免上一张图的失败计数带到新图上。
  useEffect(() => {
    retryCountRef.current = 0;
  }, [item.id, item.content, item.image_id]);

  useEffect(() => {
    let disposed = false;

    if (!isVisible) {
      // 滚出视口：卸载图片引用并释放 objectURL，压低峰值。
      setImageSrc(null);
      releaseObjectUrl();
      return undefined;
    }

    const loadImage = async () => {
      try {
        setLoading(true);
        setError(false);
        setImageSrc(null);
        releaseObjectUrl();
        setFileExists(true);
        setIsOversized(false);
        setFileSize(null);
        setFileName(null);
        setImageDimensions(null);

        if (item.content?.startsWith('data:image/')) {
          const dpr = window.devicePixelRatio || 1;
          const targetWidth = Math.max(1, Math.round((containerElementRef.current?.clientWidth || 600) * dpr));
          const targetHeight = Math.max(1, Math.round(autoMaxHeight * dpr));
          try {
            const objectUrl = await createDownsampledImageUrl(item.content, targetWidth, targetHeight);
            if (disposed) {
              URL.revokeObjectURL(objectUrl);
              return;
            }
            objectUrlRef.current = objectUrl;
            setImageSrc(objectUrl);
          } catch (decodeError) {
            // 解码失败时退回原始 data URL，交给 <img> 的 onError 走重试流程。
            if (!disposed) {
              setImageSrc(item.content);
            }
          }
          return;
        }

        if (item.content?.startsWith('files:')) {
          const filesData = parseFilesContent(item.content, item.id);
          if (filesData?.files && filesData.files.length > 0) {
            const file = filesData.files[0];
            const exists = file.exists !== false;
            const actualPath = file.actual_path || file.path;
            const size = file.size || 0;
            const name = file.name || actualPath.split(/[/\\]/).pop();

            if (disposed) {
              return;
            }

            setFileExists(exists);
            setFileSize(size);
            setFileName(name);

            if (exists) {
              const imgWidth = file.width || 0;
              const imgHeight = file.height || 0;
              const isSizeOversized = size > maxSizeBytes;
              const isDimensionOversized = (imgWidth > maxWidth || imgHeight > maxHeight) && imgWidth > 0 && imgHeight > 0;

              if (imgWidth > 0 && imgHeight > 0) {
                setImageDimensions({ width: imgWidth, height: imgHeight });
              }

              if (isSizeOversized || isDimensionOversized) {
                setIsOversized(true);
              } else {
                try {
                  const thumbnail = await getImageThumbnail({ path: actualPath, maxSide: THUMBNAIL_MAX_SIDE });
                  if (disposed) {
                    return;
                  }
                  setImageSrc(appendRetryToken(thumbnail.src, retryToken));
                } catch (thumbnailError) {
                  // 缩略图不可用（缺失/损坏/生成失败）时静默降级到原图，不弹窗。
                  if (!disposed) {
                    setImageSrc(appendRetryToken(convertFileSrc(actualPath, 'asset'), retryToken));
                  }
                }
              }
            }
            return;
          }
        }

        const imageId = parseFirstImageId(item.image_id);
        if (imageId) {
          setFileName(`${imageId}.png`);
          try {
            const thumbnail = await getImageThumbnail({ imageId, maxSide: THUMBNAIL_MAX_SIDE });
            if (disposed) {
              return;
            }
            setImageSrc(appendRetryToken(thumbnail.src, retryToken));
            return;
          } catch (thumbnailError) {
            // 缩略图不可用（原图缺失/损坏/档位异常等）：静默降级到原图路径。
            if (disposed) {
              return;
            }
          }

          const dataDir = await getDataDirectory();
          if (disposed) {
            return;
          }

          const normalizedDataDir = String(dataDir).replace(/\\/g, '/');
          const filePath = `${normalizedDataDir}/clipboard_images/${imageId}.png`;
          setImageSrc(appendRetryToken(convertFileSrc(filePath, 'asset'), retryToken));
          return;
        }

        if (!disposed) {
          setError(true);
        }
      } catch (err) {
        if (!disposed) {
          console.error('加载图片失败:', err);
          setError(true);
        }
      } finally {
        if (!disposed) {
          setLoading(false);
        }
      }
    };

    loadImage();

    return () => {
      disposed = true;
      if (imageElementRef.current) {
        // 主动断开图片资源引用，尽量让 WebView 更早释放解码缓存。
        imageElementRef.current.src = '';
      }
      releaseObjectUrl();
    };
  }, [item.id, item.content, item.image_id, retryToken, isVisible, releaseObjectUrl, autoMaxHeight, maxSizeBytes, maxWidth, maxHeight]);

  useEffect(() => {
    return () => {
      if (retryTimerRef.current) {
        clearTimeout(retryTimerRef.current);
      }
    };
  }, []);

  const scheduleLocalImageRetry = () => {
    if (!parseFirstImageId(item.image_id)) {
      setError(true);
      return;
    }
    if (retryTimerRef.current) {
      return;
    }
    const attempt = retryCountRef.current;
    if (attempt >= MAX_IMAGE_RETRIES) {
      setError(true);
      return;
    }
    const delay = Math.min(RETRY_BASE_DELAY_MS * 2 ** attempt, RETRY_MAX_DELAY_MS);
    retryCountRef.current = attempt + 1;
    retryTimerRef.current = setTimeout(() => {
      retryTimerRef.current = null;
      setRetryToken((value) => value + 1);
    }, delay);
    setError(true);
  };

  const autoContainerStyle = isAutoHeight ? { maxHeight: `${autoMaxHeight}px` } : undefined;

  if (loading) {
    return <div ref={observeContainer} className={`w-full ${isXSmallHeight ? 'h-full px-2' : 'min-h-[80px]'} bg-qc-panel-2 rounded flex items-center justify-center overflow-hidden`} style={autoContainerStyle}>
      <span className="text-sm text-qc-fg-muted">加载中...</span>
    </div>;
  }
  if (error) {
    return <div ref={observeContainer} className={`w-full ${isXSmallHeight ? 'h-full px-2' : 'min-h-[80px]'} bg-red-50 rounded flex items-center justify-center overflow-hidden`} style={autoContainerStyle}>
      <span className="text-sm text-red-500">{t('clipboard.imageLoadFailed', '图片加载失败')}</span>
    </div>;
  }
  if (isOversized || !fileExists) {
    const statusText = !fileExists
      ? t('clipboard.fileNotFound', '文件不存在')
      : t('clipboard.imageTooLarge', '图片过大');
    const sizeText = !fileExists
      ? null
      : `${formatFileSize(fileSize)} / ${maxSizeMb} MB ${t('clipboard.maxLimit', '上限')}`;
    const dimensionText = !fileExists || !imageDimensions
      ? null
      : `${imageDimensions.width} × ${imageDimensions.height}`;

    const placeholderStyle = fileExists
      ? IMAGE_PLACEHOLDER_STYLES.oversized
      : IMAGE_PLACEHOLDER_STYLES.missing;
    const iconName = !fileExists ? 'ti-photo-off' : 'ti-photo';
    const badgeIcon = !fileExists ? 'ti-x' : 'ti-alert-triangle';

    if (isXSmallHeight) {
      const summaryText = [statusText, dimensionText, fileSize ? formatFileSize(fileSize) : null]
        .filter(Boolean)
        .join(' · ');

      return (
        <div
          ref={observeContainer}
          className={'w-full h-full rounded overflow-hidden flex items-center gap-2 px-2 bg-gradient-to-br ' + placeholderStyle.gradient + ' border ' + (!fileExists ? 'opacity-60' : '')}
        >
          <div className="relative flex-shrink-0">
            <i className={'ti ' + iconName + ' text-base ' + placeholderStyle.icon} />
            <div className={'absolute -bottom-0.5 -right-0.5 ' + placeholderStyle.badge + ' rounded-full w-3 h-3 flex items-center justify-center'}>
              <i className={'ti ' + badgeIcon + ' text-white'} style={{ fontSize: 7 }} />
            </div>
          </div>
          <div className="min-w-0 flex-1 flex items-center gap-1.5 text-xs overflow-hidden">
            <Tooltip content={fileName || statusText} placement="top" asChild>
              <span className={'truncate font-medium ' + placeholderStyle.text + ' ' + (!fileExists ? 'line-through' : '')}>
                {fileName || statusText}
              </span>
            </Tooltip>
            {summaryText ? (
              <span className={'truncate ' + placeholderStyle.subText}>
                {summaryText}
              </span>
            ) : null}
          </div>
        </div>
      );
    }

    return (
      <div
        ref={observeContainer}
        className={'w-full h-full rounded overflow-hidden flex items-center bg-gradient-to-br ' + placeholderStyle.gradient + ' border ' + (isAutoHeight ? 'justify-center py-4' : 'justify-start px-3 gap-3') + ' ' + (!fileExists ? 'opacity-60' : '')}
        style={autoContainerStyle}
      >
        {isAutoHeight ? (
          <div className="flex flex-col items-center justify-center gap-2">
            <div className="relative">
              <i className={'ti ' + iconName + ' text-4xl ' + placeholderStyle.icon} />
              <div className={'absolute -bottom-1 -right-1 ' + placeholderStyle.badge + ' rounded-full w-4 h-4 flex items-center justify-center'}>
                <i className={'ti ' + badgeIcon + ' text-white text-xs'} />
              </div>
            </div>
            <div className="text-center max-w-full px-4">
              <p className={'text-sm font-medium ' + placeholderStyle.text}>
                {statusText}
              </p>
              <Tooltip content={fileName} placement="top" asChild>
                <p className={'text-xs ' + placeholderStyle.subText + ' mt-0.5 truncate'}>
                  {fileName}
                </p>
              </Tooltip>
              {(sizeText || dimensionText) && (
                <p className={'text-xs ' + placeholderStyle.subText + ' opacity-80 mt-0.5'}>
                  {[dimensionText, sizeText].filter(Boolean).join(' · ')}
                </p>
              )}
            </div>
          </div>
        ) : (
          <>
            <div className="relative flex-shrink-0">
              <i className={'ti ' + iconName + ' text-2xl ' + placeholderStyle.icon} />
              <div className={'absolute -bottom-0.5 -right-0.5 ' + placeholderStyle.badge + ' rounded-full w-3 h-3 flex items-center justify-center'}>
                <i className={'ti ' + badgeIcon + ' text-white'} style={{ fontSize: 8 }} />
              </div>
            </div>
            <div className="flex flex-col min-w-0">
              <Tooltip content={fileName} placement="top" asChild>
                <p className={'text-sm ' + placeholderStyle.text + ' truncate ' + (!fileExists ? 'line-through' : '')}>
                  {fileName}
                </p>
              </Tooltip>
              <p className={'text-xs ' + placeholderStyle.subText + ' truncate'}>
                {statusText}{dimensionText ? ` · ${dimensionText}` : ''}{sizeText ? ` · ${formatFileSize(fileSize)}` : ''}
              </p>
            </div>
          </>
        )}
      </div>
    );
  }

  return (
    <div
      ref={observeContainer}
      className={`w-full rounded overflow-hidden flex items-center justify-start bg-transparent ${isAutoHeight ? '' : 'h-full'}`}
      style={{
        contentVisibility: 'auto',
        containIntrinsicSize: '256px',
        ...(autoContainerStyle || {})
      }}
    >
      <img
        ref={imageElementRef}
        src={imageSrc}
        alt="剪贴板图片"
        className={`max-w-full object-contain pointer-events-none ${isAutoHeight ? '' : 'max-h-full'}`}
        style={isAutoHeight ? { maxHeight: `${autoMaxHeight}px` } : undefined}
        loading="lazy"
        decoding="async"
        onError={scheduleLocalImageRetry}
      />
    </div>
  );
}

export default ImageContent;
