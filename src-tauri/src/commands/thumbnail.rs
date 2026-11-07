use crate::services::get_data_directory;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::webp::WebPEncoder;
use image::ExtendedColorType;
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const THUMBNAIL_DIR: &str = "thumbnails";
const DEFAULT_MAX_SIDE: u32 = 512;
const ALLOWED_MAX_SIDES: [u32; 3] = [256, 512, 1024];
const MAX_CONCURRENT_GENERATIONS: usize = 2;
const MAX_CACHE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_CACHE_FILES: usize = 5000;
const CLEANUP_EVERY_N_GENERATIONS: u32 = 25;
const JPEG_QUALITY: u8 = 85;
/// 原图哈希记忆条目上限。仅影响命中率，超限直接清空，避免长期浏览后无限增长。
const MAX_HASH_MEMO_ENTRIES: usize = 2000;

static GENERATION_SEMAPHORE: Lazy<tokio::sync::Semaphore> =
    Lazy::new(|| tokio::sync::Semaphore::new(MAX_CONCURRENT_GENERATIONS));

/// 同一张原图 + 同一目标尺寸的并发请求在此排队，只有第一个真正解码
static IN_FLIGHT: Lazy<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// 原图内容 sha256 的内存映射，避免滚动时对同一张图反复读盘做哈希
static HASH_MEMO: Lazy<Mutex<HashMap<String, (u64, u128, String)>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

static GENERATED_COUNT: Lazy<AtomicU32> = Lazy::new(|| AtomicU32::new(0));

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThumbnailInfo {
    /// 缩略图绝对路径（原图本身不超过目标长边时直接返回原图路径）
    pub path: String,
    /// 原图绝对路径，供「点开预览取原图」直接使用
    pub original_path: String,
    pub width: u32,
    pub height: u32,
    pub max_side: u32,
    /// 命中磁盘缓存（含直接返回原图时也为 false）
    pub from_cache: bool,
    /// true 表示没有重新编码，直接返回原图路径
    pub is_original: bool,
    /// "webp" | "jpeg" | "original"
    pub format: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThumbnailCacheStats {
    pub removed_files: usize,
    pub removed_bytes: u64,
    pub total_files: usize,
    pub total_bytes: u64,
}

fn join_error(e: tokio::task::JoinError) -> String {
    format!("缩略图任务执行失败: {}", e)
}

fn thumbnails_dir() -> Result<PathBuf, String> {
    let dir = get_data_directory()?.join(THUMBNAIL_DIR);
    fs::create_dir_all(&dir).map_err(|e| format!("创建缩略图目录失败: {}: {}", dir.display(), e))?;
    Ok(dir)
}

fn resolve_source_path(image_id: Option<&str>, path: Option<&str>) -> Result<PathBuf, String> {
    if let Some(raw_path) = path.map(str::trim).filter(|p| !p.is_empty()) {
        return Ok(PathBuf::from(crate::services::resolve_stored_path(raw_path)));
    }

    if let Some(raw_id) = image_id.map(str::trim).filter(|id| !id.is_empty()) {
        if raw_id.contains('/') || raw_id.contains('\\') || raw_id.contains("..") {
            return Err(format!("image_id 非法: {}", raw_id));
        }
        let images_dir = get_data_directory()?.join("clipboard_images");
        for ext in ["png", "jpg", "jpeg", "webp", "gif"] {
            let candidate = images_dir.join(format!("{}.{}", raw_id, ext));
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
        return Err(format!("找不到 image_id 对应的原图: {}", raw_id));
    }

    Err("图片来源无效：需提供 image_id 或 path".to_string())
}

/// 记录原图哈希。条目数达到上限时整体清空，丢弃的是「缓存命中率」而不是正确性。
fn remember_hash(key: String, size: u64, modified: u128, hash: String) {
    let mut memo = HASH_MEMO.lock();
    if memo.len() >= MAX_HASH_MEMO_ENTRIES {
        memo.clear();
    }
    memo.insert(key, (size, modified, hash));
}

fn file_fingerprint(path: &Path) -> Result<String, String> {
    let meta =
        fs::metadata(path).map_err(|e| format!("读取原图信息失败: {}: {}", path.display(), e))?;
    let size = meta.len();
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let memo_key = path.to_string_lossy().to_string();

    if let Some((cached_size, cached_modified, cached_hash)) = HASH_MEMO.lock().get(&memo_key) {
        if *cached_size == size && *cached_modified == modified {
            return Ok(cached_hash.clone());
        }
    }

    let mut file =
        fs::File::open(path).map_err(|e| format!("打开原图失败: {}: {}", path.display(), e))?;
    let mut hasher = Sha256::new();
    io::copy(&mut file, &mut hasher)
        .map_err(|e| format!("读取原图失败: {}: {}", path.display(), e))?;
    let hash = hex::encode(hasher.finalize());

    remember_hash(memo_key, size, modified, hash.clone());
    Ok(hash)
}

fn find_cached(dir: &Path, key: &str) -> Option<(PathBuf, &'static str)> {
    for ext in ["webp", "jpg"] {
        let candidate = dir.join(format!("{}.{}", key, ext));
        if candidate.is_file() {
            return Some((candidate, ext));
        }
    }
    None
}

/// 命中缓存时刷新 mtime，作为 LRU 的“最后访问时间”
fn touch_mtime(path: &Path) {
    let Ok(file) = fs::OpenOptions::new().write(true).open(path) else {
        return;
    };
    let _ = file.set_modified(SystemTime::now());
}

fn generate_thumbnail(
    source: &Path,
    dir: &Path,
    key: &str,
    max_side: u32,
) -> Result<ThumbnailInfo, String> {
    let img = image::open(source)
        .map_err(|e| format!("原图解码失败: {}: {}", source.display(), e))?;

    let has_alpha = img.color().has_alpha();
    let thumb = img.thumbnail(max_side, max_side);
    let (width, height) = (thumb.width(), thumb.height());

    let (webp_bytes, color_type) = if has_alpha {
        let raw = thumb.to_rgba8();
        (raw.into_raw(), ExtendedColorType::Rgba8)
    } else {
        let raw = thumb.to_rgb8();
        (raw.into_raw(), ExtendedColorType::Rgb8)
    };

    let mut webp_buf = Vec::new();
    WebPEncoder::new_lossless(&mut webp_buf)
        .encode(&webp_bytes, width, height, color_type)
        .map_err(|e| format!("生成 WebP 缩略图失败: {}: {}", source.display(), e))?;

    // 无透明通道时同时尝试 JPEG，取体积更小的一个；有透明通道必须保留 alpha，只用 WebP
    let (encoded, format) = if has_alpha {
        (webp_buf, "webp")
    } else {
        let mut jpeg_buf = Vec::new();
        JpegEncoder::new_with_quality(&mut jpeg_buf, JPEG_QUALITY)
            .encode(&webp_bytes, width, height, ExtendedColorType::Rgb8)
            .map_err(|e| format!("生成 JPEG 缩略图失败: {}: {}", source.display(), e))?;
        if jpeg_buf.len() < webp_buf.len() {
            (jpeg_buf, "jpeg")
        } else {
            (webp_buf, "webp")
        }
    };

    let file_name = format!(
        "{}.{}",
        key,
        if format == "jpeg" { "jpg" } else { "webp" }
    );
    let final_path = dir.join(&file_name);
    let temp_path = dir.join(format!("{}.tmp", key));
    fs::write(&temp_path, &encoded)
        .map_err(|e| format!("写入缩略图失败: {}: {}", temp_path.display(), e))?;
    if final_path.exists() {
        let _ = fs::remove_file(&final_path);
    }
    fs::rename(&temp_path, &final_path).map_err(|e| {
        let _ = fs::remove_file(&temp_path);
        format!("保存缩略图失败: {}: {}", final_path.display(), e)
    })?;

    GENERATED_COUNT.fetch_add(1, Ordering::Relaxed);

    Ok(ThumbnailInfo {
        path: final_path.to_string_lossy().to_string(),
        original_path: source.to_string_lossy().to_string(),
        width,
        height,
        max_side,
        from_cache: false,
        is_original: false,
        format: format.to_string(),
    })
}

#[tauri::command]
pub async fn get_image_thumbnail(
    image_id: Option<String>,
    path: Option<String>,
    max_side: Option<u32>,
    app: tauri::AppHandle,
) -> Result<ThumbnailInfo, String> {
    let max_side = max_side.unwrap_or(DEFAULT_MAX_SIDE);
    if !ALLOWED_MAX_SIDES.contains(&max_side) {
        return Err(format!(
            "max_side 仅支持 {:?}，收到 {}",
            ALLOWED_MAX_SIDES, max_side
        ));
    }

    let source = resolve_source_path(image_id.as_deref(), path.as_deref())?;
    let source_str = source.to_string_lossy().to_string();
    if !source.is_file() {
        return Err(format!("原图不存在或不是文件: {}", source_str));
    }

    let original_size = {
        let probe = source.clone();
        tokio::task::spawn_blocking(move || image::image_dimensions(&probe))
            .await
            .map_err(join_error)?
            .map_err(|e| format!("原图无法读取或已损坏: {}: {}", source_str, e))?
    };

    // 原图本身不超过目标长边，直接返回原图，避免无意义的重新编码
    if original_size.0.max(original_size.1) <= max_side {
        // 仅对数据目录内的原图补充授权；外部路径（剪贴板里的本地文件）已由
        // get_clipboard_history / get_favorites_history 的 allow_files_content 授权，
        // 这里不做任意路径授权，避免渲染层借缩略图接口放开任意文件读取。
        if let Ok(data_dir) = get_data_directory() {
            if source.starts_with(&data_dir) {
                let _ = crate::services::asset_scope::allow_file(&app, &source_str);
            }
        }
        return Ok(ThumbnailInfo {
            path: source_str.clone(),
            original_path: source_str,
            width: original_size.0,
            height: original_size.1,
            max_side,
            from_cache: false,
            is_original: true,
            format: "original".to_string(),
        });
    }

    let dir = thumbnails_dir()?;
    let fingerprint = {
        let probe = source.clone();
        tokio::task::spawn_blocking(move || file_fingerprint(&probe))
            .await
            .map_err(join_error)??
    };
    let cache_key = format!("{}-{}", fingerprint, max_side);

    if let Some((cached, format)) = find_cached(&dir, &cache_key) {
        touch_mtime(&cached);
        let (width, height) = image::image_dimensions(&cached).unwrap_or((0, 0));
        let cached_str = cached.to_string_lossy().to_string();
        let _ = crate::services::asset_scope::allow_file(&app, &cached_str);
        return Ok(ThumbnailInfo {
            path: cached_str,
            original_path: source_str,
            width,
            height,
            max_side,
            from_cache: true,
            is_original: false,
            format: format.to_string(),
        });
    }

    // 同 key 请求合并：后来者在这里等待，醒来后重新查缓存
    let key_lock = {
        let mut map = IN_FLIGHT.lock();
        map.entry(cache_key.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    };
    let guard = key_lock.lock().await;

    if let Some((cached, format)) = find_cached(&dir, &cache_key) {
        touch_mtime(&cached);
        let (width, height) = image::image_dimensions(&cached).unwrap_or((0, 0));
        let cached_str = cached.to_string_lossy().to_string();
        let _ = crate::services::asset_scope::allow_file(&app, &cached_str);
        drop(guard);
        drop(key_lock);
        release_in_flight(&cache_key);
        return Ok(ThumbnailInfo {
            path: cached_str,
            original_path: source_str,
            width,
            height,
            max_side,
            from_cache: true,
            is_original: false,
            format: format.to_string(),
        });
    }

    // 全局并发上限：滚动时最多 2 张同时解码，其余排队
    let _permit = GENERATION_SEMAPHORE
        .acquire()
        .await
        .map_err(|_| "缩略图生成队列不可用".to_string())?;

    let generated = {
        let source_for_task = source.clone();
        let dir_for_task = dir.clone();
        let key_for_task = cache_key.clone();
        tokio::task::spawn_blocking(move || {
            generate_thumbnail(&source_for_task, &dir_for_task, &key_for_task, max_side)
        })
        .await
        .map_err(join_error)??
    };

    drop(guard);
    drop(key_lock);
    release_in_flight(&cache_key);

    let _ = crate::services::asset_scope::allow_file(&app, &generated.path);
    maybe_cleanup_in_background();

    Ok(generated)
}

fn release_in_flight(key: &str) {
    let mut map = IN_FLIGHT.lock();
    let removable = map
        .get(key)
        .map(|lock| Arc::strong_count(lock) == 1)
        .unwrap_or(false);
    if removable {
        map.remove(key);
    }
}

/// 缓存治理：超出 256MB 或 5000 条时按 mtime（最后访问时间）从旧到新淘汰
fn cleanup_cache() -> Result<ThumbnailCacheStats, String> {
    let dir = thumbnails_dir()?;
    let mut files: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
    let mut total_bytes: u64 = 0;

    let entries =
        fs::read_dir(&dir).map_err(|e| format!("读取缩略图目录失败: {}: {}", dir.display(), e))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase());
        if !matches!(ext.as_deref(), Some("webp") | Some("jpg")) {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let modified = meta.modified().unwrap_or(UNIX_EPOCH);
        total_bytes = total_bytes.saturating_add(meta.len());
        files.push((path, meta.len(), modified));
    }

    let total_files = files.len();
    let mut stats = ThumbnailCacheStats {
        removed_files: 0,
        removed_bytes: 0,
        total_files,
        total_bytes,
    };

    if total_bytes <= MAX_CACHE_BYTES && total_files <= MAX_CACHE_FILES {
        return Ok(stats);
    }

    files.sort_by_key(|(_, _, modified)| *modified);
    let mut remaining_bytes = total_bytes;
    let mut remaining_files = total_files;

    for (path, size, _) in files {
        if remaining_bytes <= MAX_CACHE_BYTES && remaining_files <= MAX_CACHE_FILES {
            break;
        }
        if fs::remove_file(&path).is_ok() {
            remaining_bytes = remaining_bytes.saturating_sub(size);
            remaining_files = remaining_files.saturating_sub(1);
            stats.removed_files += 1;
            stats.removed_bytes = stats.removed_bytes.saturating_add(size);
        }
    }

    stats.total_files = remaining_files;
    stats.total_bytes = remaining_bytes;
    Ok(stats)
}

fn maybe_cleanup_in_background() {
    let count = GENERATED_COUNT.load(Ordering::Relaxed);
    if count == 0 || count % CLEANUP_EVERY_N_GENERATIONS != 0 {
        return;
    }
    std::thread::spawn(|| {
        let _ = cleanup_cache();
    });
}

#[tauri::command]
pub async fn cleanup_thumbnail_cache() -> Result<ThumbnailCacheStats, String> {
    tokio::task::spawn_blocking(cleanup_cache)
        .await
        .map_err(join_error)?
}

/// 启动后低频执行一次清理，不阻塞 setup
pub fn schedule_startup_cleanup() {
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(3));
        let _ = cleanup_cache();
    });
}

#[cfg(test)]
mod tests {
    use super::{remember_hash, HASH_MEMO, MAX_HASH_MEMO_ENTRIES};

    #[test]
    fn hash_memo_is_bounded() {
        for index in 0..=MAX_HASH_MEMO_ENTRIES + 5 {
            remember_hash(format!("test-key-{}", index), 1, 1, "hash".to_string());
        }
        let len = HASH_MEMO.lock().len();
        assert!(len <= MAX_HASH_MEMO_ENTRIES, "HASH_MEMO 未受限: {}", len);
        HASH_MEMO.lock().clear();
    }
}
