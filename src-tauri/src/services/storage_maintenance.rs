use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::services::database::connection;
use crate::services::database::clipboard::collect_unreferenced_image_ids;
use crate::services::get_data_directory;

// 孤儿文件最少存活天数：避免删掉正在写入/刚生成还没落库的文件
const DEFAULT_MIN_AGE_DAYS: u64 = 7;
// 单次清理最多删除多少个文件
const DEFAULT_MAX_DELETE: usize = 500;
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp", "gif", "bmp", "ico", "tif", "tiff"];
const RUN_MARKER_DIR: &str = "runtime";
const RUN_MARKER_FILE: &str = "run.marker";

// 压缩进行中标记，避免两个压缩命令同时改同一个库
static COMPACT_RUNNING: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrphanCleanupStats {
    pub min_age_days: u64,
    pub max_delete: usize,
    pub scanned_files: usize,
    pub removed_clipboard_images: usize,
    pub removed_thumbnail_files: usize,
    pub removed_bytes: u64,
    pub failed_deletes: usize,
    // pin_images / image_library 属于用户自管目录，不参与删除，仅统计
    pub skipped_user_files: usize,
    // 是否因为达到单次删除上限而提前停止
    pub truncated: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactStats {
    pub before_bytes: u64,
    pub after_bytes: u64,
    pub saved_bytes: u64,
    pub auto_vacuum_before: i64,
    pub auto_vacuum_after: i64,
    pub user_version: i64,
    pub clipboard_items: i64,
    pub favorite_items: i64,
}

#[derive(Debug, Serialize, Deserialize)]
struct RunMarker {
    pid: u32,
    started_at: u64,
    consecutive_unclean: u32,
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

fn db_size(path: &Path) -> u64 {
    fs::metadata(path).map(|m| m.len()).unwrap_or(0)
        + fs::metadata(sidecar(path, "-wal")).map(|m| m.len()).unwrap_or(0)
}

fn has_image_extension(name: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| IMAGE_EXTENSIONS.iter().any(|known| known.eq_ignore_ascii_case(ext)))
        .unwrap_or(false)
}

// 递归统计用户自管目录（pin_images / image_library）的文件数，仅用于报告
fn count_user_files(dir: &Path) -> usize {
    let mut stack = vec![dir.to_path_buf()];
    let mut count = 0;
    while let Some(current) = stack.pop() {
        let entries = match fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let file_type = match entry.file_type() {
                Ok(kind) => kind,
                Err(_) => continue,
            };
            if file_type.is_dir() {
                stack.push(entry.path());
            } else if file_type.is_file() {
                count += 1;
            }
        }
    }
    count
}

// 从任意文本（content / html_content）里提取 "clipboard_images/<stem>" 形式的图片 id
// 覆盖「没有 image_id 列、但正文/富文本仍引用本地图片路径」的历史数据，避免误删
fn extract_clipboard_image_stems(text: &str) -> Vec<String> {
    ["clipboard_images/", "clipboard_images\\"]
        .iter()
        .flat_map(|marker| {
            text.match_indices(marker).filter_map(move |(pos, matched)| {
                let after = &text[pos + matched.len()..];
                let stem: String = after
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                    .collect();
                if stem.is_empty() {
                    None
                } else {
                    Some(stem)
                }
            })
        })
        .collect()
}

// 扫描正文/富文本里仍引用 clipboard_images 的 id（补充 image_id 列的盲区）
fn collect_content_referenced_image_ids(
    conn: &Connection,
    candidates: &HashSet<String>,
) -> Result<HashSet<String>, rusqlite::Error> {
    let mut referenced = HashSet::new();

    for table in ["clipboard", "favorites"] {
        let sql = format!(
            "SELECT content, COALESCE(html_content, '') FROM {} \
             WHERE content LIKE '%clipboard_images%' OR html_content LIKE '%clipboard_images%'",
            table
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (content, html_content) = row?;
            for text in [content, html_content] {
                for stem in extract_clipboard_image_stems(&text) {
                    if candidates.contains(&stem) {
                        referenced.insert(stem);
                    }
                }
            }
        }
    }

    Ok(referenced)
}

/// 扫描数据目录，回收没有被数据库引用的派生文件。
/// 只删除 7 天以前的：
/// - clipboard_images/：文件名 stem 作为 image_id，未被 clipboard/favorites 引用的孤儿
/// - thumbnails/：可随时再生的缓存，按 mtime 老化删除（含崩溃残留的 .tmp）
/// pin_images/、image_library/ 是用户自管目录，不删除，仅在统计里标记。
pub fn cleanup_orphan_files(
    min_age_days: Option<u64>,
    max_delete: Option<usize>,
) -> Result<OrphanCleanupStats, String> {
    let min_age_days = min_age_days.unwrap_or(DEFAULT_MIN_AGE_DAYS).max(1);
    let max_delete = max_delete.unwrap_or(DEFAULT_MAX_DELETE);
    let cutoff = SystemTime::now()
        .checked_sub(Duration::from_secs(min_age_days.saturating_mul(86_400)))
        .unwrap_or(UNIX_EPOCH);

    let data_dir = PathBuf::from(get_data_directory()?);
    let mut stats = OrphanCleanupStats {
        min_age_days,
        max_delete,
        scanned_files: 0,
        removed_clipboard_images: 0,
        removed_thumbnail_files: 0,
        removed_bytes: 0,
        failed_deletes: 0,
        skipped_user_files: 0,
        truncated: false,
    };
    let mut remaining = max_delete;

    // ---- clipboard_images：按数据库 image_id 引用判定 ----
    let clipboard_dir = data_dir.join("clipboard_images");
    let mut clipboard_entries: HashMap<String, (PathBuf, SystemTime, u64)> = HashMap::new();
    let mut candidate_ids: HashSet<String> = HashSet::new();
    if let Ok(entries) = fs::read_dir(&clipboard_dir) {
        for entry in entries.flatten() {
            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };
            if !metadata.is_file() {
                continue;
            }
            stats.scanned_files += 1;
            let name = entry.file_name().to_string_lossy().to_string();
            if !has_image_extension(&name) {
                continue;
            }
            let stem = match Path::new(&name).file_stem().and_then(|stem| stem.to_str()) {
                Some(stem) => stem.to_string(),
                None => continue,
            };
            candidate_ids.insert(stem.clone());
            clipboard_entries.insert(
                stem,
                (
                    entry.path(),
                    metadata.modified().unwrap_or(UNIX_EPOCH),
                    metadata.len(),
                ),
            );
        }
    }

    if !candidate_ids.is_empty() {
        let (unreferenced, content_referenced) = connection::with_connection(|conn| {
            let unreferenced =
                collect_unreferenced_image_ids(conn, candidate_ids.clone())?;
            let content_referenced =
                collect_content_referenced_image_ids(conn, &candidate_ids)?;
            Ok((unreferenced, content_referenced))
        })
        .map_err(|error| format!("查询图片引用失败: {}", error))?;

        let mut orphans: Vec<(PathBuf, SystemTime, u64)> = unreferenced
            .into_iter()
            .filter(|image_id| !content_referenced.contains(image_id))
            .filter_map(|image_id| clipboard_entries.get(&image_id).cloned())
            .filter(|(_, modified, _)| *modified < cutoff)
            .collect();
        orphans.sort_by_key(|(_, modified, _)| *modified);

        for (path, _, length) in orphans {
            if remaining == 0 {
                stats.truncated = true;
                break;
            }
            match fs::remove_file(&path) {
                Ok(()) => {
                    remaining = remaining.saturating_sub(1);
                    stats.removed_clipboard_images += 1;
                    stats.removed_bytes += length;
                }
                Err(error) => {
                    stats.failed_deletes += 1;
                    eprintln!("[orphan] 删除失败 {}: {}", path.display(), error);
                }
            }
        }
    }

    // ---- thumbnails：纯派生缓存，按最后修改时间老化 ----
    let thumbnail_dir = data_dir.join("thumbnails");
    let mut thumbnails: Vec<(PathBuf, SystemTime, u64)> = Vec::new();
    if let Ok(entries) = fs::read_dir(&thumbnail_dir) {
        for entry in entries.flatten() {
            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };
            if !metadata.is_file() {
                continue;
            }
            stats.scanned_files += 1;
            let name = entry.file_name().to_string_lossy().to_string();
            let is_thumbnail = name.ends_with(".tmp")
                || matches!(
                    Path::new(&name).extension().and_then(|ext| ext.to_str()),
                    Some(ext) if ext.eq_ignore_ascii_case("webp") || ext.eq_ignore_ascii_case("jpg") || ext.eq_ignore_ascii_case("jpeg")
                );
            if !is_thumbnail {
                continue;
            }
            let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
            if modified < cutoff {
                thumbnails.push((entry.path(), modified, metadata.len()));
            }
        }
    }
    thumbnails.sort_by_key(|(_, modified, _)| *modified);

    for (path, _, length) in thumbnails {
        if remaining == 0 {
            stats.truncated = true;
            break;
        }
        match fs::remove_file(&path) {
            Ok(()) => {
                remaining = remaining.saturating_sub(1);
                stats.removed_thumbnail_files += 1;
                stats.removed_bytes += length;
            }
            Err(error) => {
                stats.failed_deletes += 1;
                eprintln!("[orphan] 删除失败 {}: {}", path.display(), error);
            }
        }
    }

    // ---- 用户自管目录：只统计，不删除 ----
    let user_files = count_user_files(&data_dir.join("pin_images"))
        + count_user_files(&data_dir.join("image_library"));
    stats.scanned_files += user_files;
    stats.skipped_user_files = user_files;

    Ok(stats)
}

/// 启动后延迟数秒在后台执行一次；不在每次复制时触发。
pub fn schedule_startup_orphan_cleanup() {
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(8));
        match cleanup_orphan_files(None, None) {
            Ok(stats) => {
                if stats.removed_clipboard_images > 0 || stats.removed_thumbnail_files > 0 {
                    eprintln!(
                        "[orphan] 启动清理完成：剪贴板图片 {} 个、缩略图 {} 个，释放 {} 字节，失败 {} 个{}",
                        stats.removed_clipboard_images,
                        stats.removed_thumbnail_files,
                        stats.removed_bytes,
                        stats.failed_deletes,
                        if stats.truncated { "（达到单次上限，剩余下次处理）" } else { "" }
                    );
                }
            }
            Err(error) => eprintln!("[orphan] 启动清理失败: {}", error),
        }
    });
}

// 压缩期间拒绝新的数据库操作，并在不阻塞剪贴板监听线程的前提下丢弃采集
struct CompactGuard;

impl Drop for CompactGuard {
    fn drop(&mut self) {
        connection::set_maintenance_mode(false);
        COMPACT_RUNNING.store(false, Ordering::SeqCst);
    }
}

/// 用 VACUUM INTO 生成紧凑副本 → 校验 → 原子替换。
/// 老库 auto_vacuum=0 时顺带把副本迁移到 INCREMENTAL（不改 user_version）。
/// 任一步失败都保留/恢复原库，不会出现替换一半的状态。
pub fn compact_database() -> Result<CompactStats, String> {
    if COMPACT_RUNNING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("数据库压缩正在进行中，请稍后再试".to_string());
    }

    connection::set_maintenance_mode(true);
    let _guard = CompactGuard;
    // 压缩期间 capture 主动丢弃；结束后再多等 1.5s 防止新内容刚好落在替换窗口
    let _pause = crate::services::clipboard::pause_clipboard_monitor_for(1_500);

    compact_database_inner()
}

fn compact_database_inner() -> Result<CompactStats, String> {
    let data_dir = PathBuf::from(get_data_directory()?);
    let db_path = data_dir.join("quickclipboard.db");
    if !db_path.is_file() {
        return Err(format!("数据库文件不存在: {}", db_path.display()));
    }
    let db_str = db_path
        .to_str()
        .ok_or("数据库路径无效")?
        .to_string();
    let new_path = data_dir.join(format!(
        "quickclipboard.compact-{}.tmp",
        std::process::id()
    ));
    remove_database_files(&new_path);

    let before_bytes = db_size(&db_path);

    // 先释放全局连接，才能用独立连接 checkpoint + VACUUM INTO
    connection::close_database();

    let staged = (|| -> Result<(i64, i64, i64, i64), String> {
        let conn = Connection::open(&db_str).map_err(|e| format!("打开数据库失败: {}", e))?;
        let _ = conn.busy_timeout(Duration::from_secs(10));
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .map_err(|e| format!("WAL 检查点失败: {}", e))?;

        let auto_vacuum_before: i64 = conn
            .query_row("PRAGMA auto_vacuum", [], |row| row.get(0))
            .map_err(|e| format!("读取 auto_vacuum 失败: {}", e))?;
        let user_version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|e| format!("读取 user_version 失败: {}", e))?;
        let clipboard_items: i64 = conn
            .query_row("SELECT COUNT(*) FROM clipboard", [], |row| row.get(0))
            .map_err(|e| format!("统计剪贴板条数失败: {}", e))?;
        let favorite_items: i64 = conn
            .query_row("SELECT COUNT(*) FROM favorites", [], |row| row.get(0))
            .map_err(|e| format!("统计收藏条数失败: {}", e))?;

        let escaped = new_path.to_string_lossy().replace('\'', "''");
        conn.execute_batch(&format!("VACUUM INTO '{}'", escaped))
            .map_err(|e| format!("VACUUM INTO 失败: {}", e))?;
        drop(conn);

        Ok((auto_vacuum_before, user_version, clipboard_items, favorite_items))
    })();

    let (auto_vacuum_before, user_version, clipboard_items, favorite_items) = match staged {
        Ok(values) => values,
        Err(error) => {
            remove_database_files(&new_path);
            let _ = connection::init_database(&db_str);
            return Err(format!("数据库压缩失败，原库未变更: {}", error));
        }
    };

    // 老库 auto_vacuum=0：对新副本开启 INCREMENTAL 后再 VACUUM 一次，完成一次性迁移
    let mut auto_vacuum_after = auto_vacuum_before;
    if auto_vacuum_before == 0 {
        let migrated = (|| -> Result<(), String> {
            let conn = Connection::open(&new_path).map_err(|e| format!("打开压缩副本失败: {}", e))?;
            conn.execute_batch("PRAGMA auto_vacuum = INCREMENTAL; VACUUM;")
                .map_err(|e| format!("迁移 auto_vacuum 失败: {}", e))?;
            Ok(())
        })();
        if let Err(error) = migrated {
            remove_database_files(&new_path);
            let _ = connection::init_database(&db_str);
            return Err(format!("数据库压缩失败，原库未变更: {}", error));
        }
        auto_vacuum_after = 2;
    }

    // 校验副本：完整性、user_version、行数
    let validated = (|| -> Result<(), String> {
        let conn = Connection::open(&new_path).map_err(|e| format!("校验副本打开失败: {}", e))?;
        conn.execute_batch("PRAGMA query_only = ON;")
            .map_err(|e| format!("设置只读校验失败: {}", e))?;
        let integrity: String = conn
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .map_err(|e| format!("完整性检查失败: {}", e))?;
        if integrity != "ok" {
            return Err(format!("完整性检查未通过: {}", integrity));
        }
        let copy_version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|e| format!("读取副本 user_version 失败: {}", e))?;
        if copy_version != user_version {
            return Err(format!(
                "user_version 不一致：原库 {}，副本 {}",
                user_version, copy_version
            ));
        }
        let copy_clipboard: i64 = conn
            .query_row("SELECT COUNT(*) FROM clipboard", [], |row| row.get(0))
            .map_err(|e| format!("统计副本剪贴板失败: {}", e))?;
        if copy_clipboard != clipboard_items {
            return Err(format!(
                "剪贴板条数不一致：原库 {}，副本 {}",
                clipboard_items, copy_clipboard
            ));
        }
        let copy_favorites: i64 = conn
            .query_row("SELECT COUNT(*) FROM favorites", [], |row| row.get(0))
            .map_err(|e| format!("统计副本收藏失败: {}", e))?;
        if copy_favorites != favorite_items {
            return Err(format!(
                "收藏条数不一致：原库 {}，副本 {}",
                favorite_items, copy_favorites
            ));
        }
        drop(conn);
        Ok(())
    })();

    if let Err(error) = validated {
        remove_database_files(&new_path);
        let _ = connection::init_database(&db_str);
        return Err(format!("数据库压缩失败，原库未变更: {}", error));
    }

    // 原子替换：原库先改名备份，再换入副本；任一步失败都恢复备份
    let stamp = unix_now();
    let backup = data_dir.join(format!("quickclipboard.db.compact-bak-{}", stamp));
    if let Err(error) = swap_database_files(&db_path, &new_path, &backup) {
        remove_database_files(&new_path);
        let _ = connection::init_database(&db_str);
        return Err(format!("数据库替换失败，已恢复原库: {}", error));
    }

    match connection::init_database(&db_str) {
        Ok(()) => {
            remove_backup_files(&backup);
            let after_bytes = db_size(&db_path);
            Ok(CompactStats {
                before_bytes,
                after_bytes,
                saved_bytes: before_bytes.saturating_sub(after_bytes),
                auto_vacuum_before,
                auto_vacuum_after,
                user_version,
                clipboard_items,
                favorite_items,
            })
        }
        Err(error) => {
            // 新库打不开：恢复备份并重新打开原库
            restore_backup_files(&db_path, &backup);
            let _ = connection::init_database(&db_str);
            Err(format!("新数据库打开失败，已回滚原库: {}", error))
        }
    }
}

fn remove_database_files(path: &Path) {
    let _ = fs::remove_file(path);
    for suffix in ["-wal", "-shm"] {
        let _ = fs::remove_file(sidecar(path, suffix));
    }
}

fn swap_database_files(db: &Path, new_db: &Path, backup: &Path) -> Result<(), String> {
    fs::rename(db, backup).map_err(|e| format!("备份原库失败: {}", e))?;

    for suffix in ["-wal", "-shm"] {
        let source = sidecar(db, suffix);
        if source.exists() {
            let target = sidecar(backup, suffix);
            if let Err(error) = fs::rename(&source, &target) {
                restore_backup_files(db, backup);
                return Err(format!("备份 {} 失败: {}", suffix, error));
            }
        }
    }

    if let Err(error) = fs::rename(new_db, db) {
        restore_backup_files(db, backup);
        return Err(format!("换入压缩副本失败: {}", error));
    }
    // 副本自身的 -wal/-shm 不应残留，校验连接已正常关闭，可直接清理
    for suffix in ["-wal", "-shm"] {
        let _ = fs::remove_file(sidecar(new_db, suffix));
    }
    Ok(())
}

fn restore_backup_files(db: &Path, backup: &Path) {
    if !backup.exists() {
        return;
    }
    let _ = fs::remove_file(db);
    let _ = fs::rename(backup, db);
    for suffix in ["-wal", "-shm"] {
        let source = sidecar(backup, suffix);
        if source.exists() {
            let target = sidecar(db, suffix);
            let _ = fs::remove_file(&target);
            let _ = fs::rename(&source, &target);
        }
    }
}

fn remove_backup_files(backup: &Path) {
    let _ = fs::remove_file(backup);
    for suffix in ["-wal", "-shm"] {
        let _ = fs::remove_file(sidecar(backup, suffix));
    }
}

// 清理压缩过程遗留的临时副本（quickclipboard.compact-<pid>.tmp 及其 sidecar）
fn cleanup_stale_compact_tmp(data_dir: &Path) {
    if let Ok(entries) = fs::read_dir(data_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("quickclipboard.compact-") {
                let _ = fs::remove_file(entry.path());
                for suffix in ["-wal", "-shm"] {
                    let _ = fs::remove_file(sidecar(&entry.path(), suffix));
                }
            }
        }
    }
}

/// 启动时修复「压缩过程中被强制结束」留下的半成品状态：
/// - 主库不存在但有 quickclipboard.db.compact-bak-*：用最新备份恢复主库
/// - 主库存在：只清理 .tmp 副本，备份保留到 `init_database` 成功后再删
/// 返回 Some(说明) 表示做过修复，调用方可写入诊断日志。
///
/// 备份清理刻意延后：若主库存在但打不开，`init_database` 会走备份恢复路径，
/// 此时这些 `.compact-bak-*` 是需要保留的最后回滚点。
pub fn recover_interrupted_compact() -> Option<String> {
    let data_dir = PathBuf::from(get_data_directory().ok()?);
    let db_path = data_dir.join("quickclipboard.db");

    let mut backups: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = fs::read_dir(&data_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("quickclipboard.db.compact-bak-") {
                backups.push(entry.path());
            }
        }
    }
    backups.sort();

    cleanup_stale_compact_tmp(&data_dir);

    if !db_path.exists() {
        // 替换窗口内被强杀：主库已改名备份但新库尚未就位
        if let Some(backup) = backups.last().cloned() {
            restore_backup_files(&db_path, &backup);
            // 其余备份不在此处删除，等主库确认可用后再统一清理
            return Some("检测到上次压缩中断，已用最近备份恢复数据库".to_string());
        }
    }

    None
}

/// 主库确认可用（`init_database` 成功）之后调用，清理上一轮压缩残留的备份。
pub fn cleanup_stale_compact_backups() {
    let Ok(dir) = get_data_directory() else {
        return;
    };
    let data_dir = PathBuf::from(dir);
    let Ok(entries) = fs::read_dir(&data_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with("quickclipboard.db.compact-bak-") {
            remove_backup_files(&entry.path());
        }
    }
}

/// 启动时写「运行中」标记；若发现上一次运行留下的标记未清除，累加异常退出次数。
/// 连续 2 次才写诊断日志，不自动重建数据库。
pub fn init_run_marker() {
    let data_dir = match get_data_directory() {
        Ok(dir) => PathBuf::from(dir),
        Err(_) => return,
    };
    let dir = data_dir.join(RUN_MARKER_DIR);
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join(RUN_MARKER_FILE);
    let previous = fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str::<RunMarker>(&text).ok());

    let consecutive_unclean = previous
        .map(|marker| marker.consecutive_unclean.saturating_add(1))
        .unwrap_or(0);

    if consecutive_unclean >= 2 {
        let message = format!(
            "连续 {} 次启动检测到上次运行标记未清除，应用可能异常退出；仅作为诊断依据，不自动重建数据库",
            consecutive_unclean
        );
        eprintln!("[run-marker] {}", message);
        crate::startup_diagnostics::append_database_recovery_log(&message);
    }

    let marker = RunMarker {
        pid: std::process::id(),
        started_at: unix_now(),
        consecutive_unclean,
    };
    if let Ok(json) = serde_json::to_string(&marker) {
        let _ = fs::write(&path, json);
    }
}

/// 正常退出时清除标记；强制结束/崩溃时标记会残留，供下次启动判定。
pub fn clear_run_marker() {
    if let Ok(dir) = get_data_directory() {
        let _ = fs::remove_file(PathBuf::from(dir).join(RUN_MARKER_DIR).join(RUN_MARKER_FILE));
    }
}
