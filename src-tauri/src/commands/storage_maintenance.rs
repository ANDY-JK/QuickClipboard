use crate::services::storage_maintenance::{
    cleanup_orphan_files, compact_database as compact_database_impl, CompactStats,
    OrphanCleanupStats,
};

/// 手动触发一次孤儿文件回收。
/// 只删除 7 天以前的未引用剪贴板图片与老化缩略图；pin_images / image_library 不动。
#[tauri::command]
pub async fn cleanup_orphan_images(
    min_age_days: Option<u64>,
    max_delete: Option<usize>,
) -> Result<OrphanCleanupStats, String> {
    tauri::async_runtime::spawn_blocking(move || cleanup_orphan_files(min_age_days, max_delete))
        .await
        .map_err(|e| format!("任务执行失败: {}", e))?
}

/// 显式压缩数据库：VACUUM INTO → 校验 → 原子替换，返回压缩前后字节数。
/// 执行期间暂停剪贴板采集，其他数据库操作会收到「维护中」错误。
#[tauri::command]
pub async fn compact_database() -> Result<CompactStats, String> {
    tauri::async_runtime::spawn_blocking(compact_database_impl)
        .await
        .map_err(|e| format!("任务执行失败: {}", e))?
}
