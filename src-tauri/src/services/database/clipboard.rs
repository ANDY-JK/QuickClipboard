use super::models::{ClipboardDataItem, ClipboardDataSeed, ClipboardItem, PaginatedResult, QueryParams};
use super::connection::{maybe_shrink_after_bulk_delete, with_connection, MAX_CONTENT_LENGTH};
use crate::utils::{is_textual_content_type, truncate_string, truncate_around_keyword, truncate_html};
use rusqlite::{params, OptionalExtension};
use std::collections::HashSet;
use chrono;
use uuid::Uuid;

// 大体积文件/图片列表返回前剥离 base64 图标，避免数十 MB 走 IPC
const MAX_INLINE_FILE_ICON_JSON: usize = 64 * 1024;

fn strip_file_icons_if_large(content: String, content_type: &str) -> String {
    if content.len() <= MAX_INLINE_FILE_ICON_JSON
        || !(content_type.contains("file") || content_type.contains("image"))
    {
        return content;
    }
    let json = match content.strip_prefix("files:") {
        Some(json) => json,
        None => return content,
    };
    let mut data = match serde_json::from_str::<crate::services::paste::FilesData>(json) {
        Ok(data) => data,
        Err(_) => return content,
    };
    let mut changed = false;
    for file in &mut data.files {
        if file.icon_data.take().is_some() {
            changed = true;
        }
    }
    if !changed {
        return content;
    }
    match serde_json::to_string(&data) {
        Ok(serialized) => format!("files:{}", serialized),
        Err(_) => content,
    }
}


pub fn save_clipboard_data_items(
    target_kind: &str,
    target_id: &str,
    items: &[ClipboardDataSeed],
) -> Result<(), String> {
    if items.is_empty() {
        return Ok(());
    }

    with_connection(|conn| {
        let now = chrono::Local::now().timestamp();
        let tx = conn.unchecked_transaction()?;

        for item in items {
            tx.execute(
                "INSERT INTO clipboard_data (
                    target_kind, target_id, format_name, raw_data,
                    is_primary, format_order, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(target_kind, target_id, format_name)
                 DO UPDATE SET
                    raw_data = excluded.raw_data,
                    is_primary = excluded.is_primary,
                    format_order = excluded.format_order,
                    updated_at = excluded.updated_at",
                params![
                    target_kind,
                    target_id,
                    item.format_name,
                    item.raw_data,
                    if item.is_primary { 1 } else { 0 },
                    item.format_order,
                    now,
                    now,
                ],
            )?;
        }

        tx.commit()?;
        Ok(())
    })
}

fn get_clipboard_data_items_by_target(
    target_kind: &str,
    target_id: &str,
) -> Result<Vec<ClipboardDataItem>, String> {
    with_connection(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, target_kind, target_id, format_name, raw_data, is_primary, format_order, created_at, updated_at
             FROM clipboard_data
             WHERE target_kind = ?1 AND target_id = ?2
             ORDER BY format_order ASC, id ASC",
        )?;

        let items = stmt
            .query_map(params![target_kind, target_id], |row| {
                Ok(ClipboardDataItem {
                    id: row.get(0)?,
                    target_kind: row.get(1)?,
                    target_id: row.get(2)?,
                    format_name: row.get(3)?,
                    raw_data: row.get(4)?,
                    is_primary: row.get::<_, i64>(5)? != 0,
                    format_order: row.get(6)?,
                    created_at: row.get(7)?,
                    updated_at: row.get(8)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(items)
    })
}

pub fn get_clipboard_data_items(
    target_kind: &str,
    target_id: &str,
) -> Result<Vec<ClipboardDataItem>, String> {
    get_clipboard_data_items_by_target(target_kind, target_id)
}

pub fn delete_clipboard_data_items(target_kind: &str, target_id: &str) -> Result<(), String> {
    with_connection(|conn| {
        conn.execute(
            "DELETE FROM clipboard_data WHERE target_kind = ?1 AND target_id = ?2",
            params![target_kind, target_id],
        )?;
        Ok(())
    })
}

// 异步更新缺失的字符数（直接按 SQL 计算，无需把全文读入内存）
pub fn update_missing_char_counts(ids: Vec<i64>) {
    if ids.is_empty() { return; }
    
    std::thread::spawn(move || {
        let _ = with_connection(|conn| {
            for id in ids {
                conn.execute(
                    "UPDATE clipboard SET char_count = length(content) WHERE id = ?1 AND char_count IS NULL",
                    params![id],
                )?;
            }
            Ok(())
        });
    });
}

// 按逗号拆分图片ID
fn split_image_ids(s: &str) -> Vec<String> {
    s.split(',')
        .map(|x| x.trim())
        .filter(|x| !x.is_empty())
        .map(|x| x.to_string())
        .collect()
}

// 计算用于精确去重的 content_hash（写入路径与历史回填共用，保证两侧口径一致）
// 文本: 对 content 全文做 SHA256；文件/图片: 对排序后的文件路径集合做 SHA256。
pub fn compute_content_hash(content: &str, content_type: &str) -> String {
    use sha2::{Digest, Sha256};

    let is_text = content_type.starts_with("text")
        || content_type.contains("rich_text")
        || content_type.contains("link");
    let is_file = !is_text && (content_type.contains("image") || content_type.contains("file"));

    if !is_text && !is_file {
        // 不可去重类型：写空串哨兵（非 NULL，保证回填迁移能推进；也不会与有效哈希匹配）
        return String::new();
    }

    let mut hasher = Sha256::new();
    hasher.update(if is_text { b"text\0" } else { b"file\0" });

    if is_file {
        for path in extract_file_paths_from_content(content) {
            hasher.update(path.as_bytes());
            hasher.update([0u8]);
        }
    } else {
        hasher.update(content.as_bytes());
    }

    format!("{:x}", hasher.finalize())
}

// 从 files: JSON 提取并排序文件路径；解析失败时退化为整段内容
fn extract_file_paths_from_content(content: &str) -> Vec<String> {
    if let Some(json) = content.strip_prefix("files:") {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(json) {
            let mut paths: Vec<String> = value["files"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|file| file["path"].as_str().map(String::from))
                .collect();
            paths.sort();
            return paths;
        }
    }
    vec![content.to_string()]
}

// 一次性收集候选图片 ID 中仍被 clipboard/favorites 引用的部分
// （替代逐 ID、每 ID 8 次 LIKE 的引用判定）
pub(crate) fn collect_unreferenced_image_ids(
    conn: &rusqlite::Connection,
    candidates: HashSet<String>,
) -> Result<Vec<String>, rusqlite::Error> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let mut referenced: HashSet<String> = HashSet::new();

    for table in ["clipboard", "favorites"] {
        let sql = format!(
            "SELECT image_id FROM {} WHERE image_id IS NOT NULL AND image_id <> ''",
            table
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        for row in rows {
            for image_id in split_image_ids(&row?) {
                if candidates.contains(&image_id) {
                    referenced.insert(image_id);
                }
            }
        }
    }

    Ok(candidates
        .into_iter()
        .filter(|image_id| !referenced.contains(image_id))
        .collect())
}

// 检查图片ID是否仍被 clipboard 或 favorites 引用
fn is_image_id_referenced(conn: &rusqlite::Connection, image_id: &str) -> Result<bool, rusqlite::Error> {
    let exact = image_id;
    let p1 = format!("{},%", image_id);
    let p2 = format!("%,{},%", image_id);
    let p3 = format!("%,{}", image_id);

    let q = |table: &str| -> Result<bool, rusqlite::Error> {
        let sql = format!(
            "SELECT EXISTS(SELECT 1 FROM {} WHERE image_id = ?1 OR image_id LIKE ?2 OR image_id LIKE ?3 OR image_id LIKE ?4)",
            table
        );
        let exists: i64 = conn.query_row(&sql, params![exact, p1, p2, p3], |row| row.get(0))?;
        Ok(exists != 0)
    };

    Ok(q("clipboard")? || q("favorites")?)
}

// 删除图片文件
fn delete_image_files(image_ids: Vec<String>) -> Result<(), String> {
    if image_ids.is_empty() { return Ok(()); }
    let data_dir = crate::services::get_data_directory()?;
    let images_dir = data_dir.join("clipboard_images");
    for iid in image_ids {
        let p = images_dir.join(format!("{}.png", iid));
        if p.exists() {
            let _ = std::fs::remove_file(&p);
        }
    }
    Ok(())
}

// 分页查询剪贴板历史
pub fn query_clipboard_items(params: QueryParams) -> Result<PaginatedResult<ClipboardItem>, String> {
    let search_keyword = params.search.clone();
    let has_filter = search_keyword.as_ref().map(|s| !s.trim().is_empty()).unwrap_or(false)
        || params.content_type.as_ref().map(|t| t != "all").unwrap_or(false)
        || params.paste_status.as_ref().map(|s| s.split(',').any(|v| v.trim() == "pasted" || v.trim() == "unpasted")).unwrap_or(false);
    
    with_connection(|conn| {
        let mut where_clauses: Vec<String> = vec![];
        let mut query_params: Vec<Box<dyn rusqlite::ToSql>> = vec![];
        
        if let Some(ref search) = search_keyword {
            if !search.trim().is_empty() {
                where_clauses.push("content LIKE ?".to_string());
                let search_pattern = format!("%{}%", search);
                query_params.push(Box::new(search_pattern));
            }
        }
        
        if let Some(ref content_type) = params.content_type {
            let types: Vec<_> = content_type.split(',').map(str::trim).filter(|t| !t.is_empty()).collect();
            if !types.is_empty() && content_type != "all" {
                let clauses = types.iter().map(|_| "content_type LIKE ?").collect::<Vec<_>>().join(" OR ");
                where_clauses.push(format!("({})", clauses));
                for content_type in types {
                    let pattern = if content_type == "text" { "%text%".to_string() } else { format!("%{}%", content_type) };
                    query_params.push(Box::new(pattern));
                }
            }
        }
        if let Some(ref paste_status) = params.paste_status {
            let statuses: Vec<_> = paste_status.split(',').map(str::trim).filter(|status| *status == "pasted" || *status == "unpasted").collect();
            if statuses.len() == 1 {
                where_clauses.push(if statuses[0] == "pasted" { "paste_count > 0".to_string() } else { "paste_count = 0".to_string() });
            }
        }
        
        let where_clause = if where_clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", where_clauses.join(" AND "))
        };
        
        let total_count: i64 = if has_filter {
            let count_sql = format!("SELECT COUNT(*) FROM clipboard {}", where_clause);
            let count_params: Vec<Box<dyn rusqlite::ToSql>> = query_params.iter().map(|p| {
                let val: Box<dyn rusqlite::ToSql> = match p.as_ref().to_sql() {
                    Ok(rusqlite::types::ToSqlOutput::Borrowed(rusqlite::types::ValueRef::Text(s))) => {
                        Box::new(String::from_utf8_lossy(s).to_string())
                    }
                    _ => Box::new("")
                };
                val
            }).collect();
            conn.query_row(
                &count_sql,
                rusqlite::params_from_iter(count_params.iter().map(|p| p.as_ref())),
                |row| row.get(0)
            )?
        } else {
            conn.query_row("SELECT COUNT(*) FROM clipboard", [], |row| row.get(0))?
        };
        
        if total_count == 0 {
            return Ok(PaginatedResult::new(0, vec![], params.offset, params.limit));
        }
        
        let has_keyword_search = search_keyword
            .as_ref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        // 无关键词时把文本截断下推到 SQL，避免把整段历史内容读入内存
        let content_expr = if has_keyword_search {
            "content"
        } else {
            "CASE WHEN content_type LIKE '%text%' OR content_type LIKE '%link%' THEN substr(content, 1, 1600) ELSE content END"
        };
        let query_sql = format!(
            "SELECT id, uuid, source_device_id, is_remote, {}, html_content, content_type, image_id, item_order, is_pinned, paste_count, source_app, source_icon_hash, created_at, updated_at, char_count,
                    (SELECT id FROM favorites WHERE source_clipboard_uuid = clipboard.uuid LIMIT 1) AS favorite_id,
                    char_count IS NULL AS char_count_missing,
                    length(content) AS content_char_len
             FROM clipboard 
             {} 
             ORDER BY is_pinned DESC, item_order DESC, updated_at DESC 
             LIMIT ? OFFSET ?",
            content_expr, where_clause
        );
        
        query_params.push(Box::new(params.limit));
        query_params.push(Box::new(params.offset));
        
        let mut stmt = conn.prepare(&query_sql)?;

        let mut ids_to_update: Vec<i64> = vec![];
        
        let items = stmt.query_map(
            rusqlite::params_from_iter(query_params.iter().map(|p| p.as_ref())),
            |row| {
                let id: i64 = row.get(0)?;
                let uuid: Option<String> = row.get(1)?;
                let source_device_id: Option<String> = row.get(2)?;
                let is_remote: i64 = row.get(3)?;
                let content: String = row.get(4)?;
                let html_content: Option<String> = row.get(5)?;
                let content_type: String = row.get(6)?;
                let char_count: Option<i64> = row.get(15)?;
                let char_count_missing: i64 = row.get(17)?;
                let content_char_len: i64 = row.get(18)?;
                
                let needs_char_count = content_type.contains("text") || content_type.contains("rich_text");
                let missing_char_count = char_count_missing != 0 && needs_char_count && content_char_len > 0;
                let final_char_count = if missing_char_count {
                    Some(content_char_len)
                } else {
                    char_count
                };
                // 缺失字符数由 SQL 直接按 id 回填，无需把全文留在内存里
                let id_for_update = if missing_char_count { Some(id) } else { None };

                let (truncated_content, truncated_html) = if is_textual_content_type(&content_type) {
                    let truncated_content = if has_keyword_search {
                        if content.len() > MAX_CONTENT_LENGTH {
                            if let Some(ref keyword) = search_keyword {
                                if !keyword.trim().is_empty() {
                                    truncate_around_keyword(content, keyword, MAX_CONTENT_LENGTH)
                                } else {
                                    truncate_string(content, MAX_CONTENT_LENGTH)
                                }
                            } else {
                                truncate_string(content, MAX_CONTENT_LENGTH)
                            }
                        } else {
                            content
                        }
                    } else if content_char_len as usize > MAX_CONTENT_LENGTH {
                        // 内容已由 SQL 截到 1600 字符，这里只补上原有提示后缀
                        format!("{}...(内容过长已截断)", content)
                    } else {
                        content
                    };
                    
                    let truncated_html = html_content.map(|h| {
                        if h.len() > MAX_CONTENT_LENGTH {
                            truncate_html(h, MAX_CONTENT_LENGTH)
                        } else {
                            h
                        }
                    });
                    
                    (truncated_content, truncated_html)
                } else {
                    (strip_file_icons_if_large(content, &content_type), html_content)
                };
                
                Ok((ClipboardItem {
                    id,
                    uuid,
                    favorite_id: row.get(16)?,
                    source_device_id,
                    is_remote: is_remote != 0,
                    content: truncated_content,
                    html_content: truncated_html,
                    content_type: content_type.clone(),
                    image_id: row.get(7)?,
                    item_order: row.get(8)?,
                    is_pinned: row.get::<_, i64>(9)? != 0,
                    paste_count: row.get(10)?,
                    source_app: row.get(11)?,
                    source_icon_hash: row.get(12)?,
                    char_count: final_char_count,
                    created_at: row.get(13)?,
                    updated_at: row.get(14)?,
                }, id_for_update))
            }
        )?
        .collect::<Result<Vec<_>, _>>()?;
        
        let mut result_items = vec![];
        for (item, id_to_update) in items {
            if let Some(id) = id_to_update {
                ids_to_update.push(id);
            }
            result_items.push(item);
        }

        if !ids_to_update.is_empty() {
            update_missing_char_counts(ids_to_update);
        }
        
        Ok(PaginatedResult::new(total_count, result_items, params.offset, params.limit))
    })
}

// 获取剪贴板总数
pub fn get_clipboard_count() -> Result<i64, String> {
    with_connection(|conn| {
        conn.query_row("SELECT COUNT(*) FROM clipboard", [], |row| row.get(0))
    })
}

pub fn get_clipboard_item_position(id: i64) -> Result<Option<i64>, String> {
    with_connection(|conn| {
        let target: Option<(i64, i64, i64)> = conn
            .query_row(
                "SELECT is_pinned, item_order, updated_at FROM clipboard WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;

        let Some((is_pinned, item_order, updated_at)) = target else {
            return Ok(None);
        };

        // 与列表排序口径一致：is_pinned DESC, item_order DESC, updated_at DESC
        let position: i64 = conn.query_row(
            "SELECT COUNT(*) FROM clipboard
             WHERE is_pinned > ?1
                OR (is_pinned = ?1 AND item_order > ?2)
                OR (is_pinned = ?1 AND item_order = ?2 AND updated_at > ?3)",
            params![is_pinned, item_order, updated_at],
            |row| row.get(0),
        )?;

        Ok(Some(position))
    })
}

// 根据ID获取剪贴板项（完整内容，不截断）
pub fn get_clipboard_item_by_id(id: i64) -> Result<Option<ClipboardItem>, String> {
    get_clipboard_item_by_id_with_limit(id, None)
}

pub fn ensure_clipboard_item_uuid(id: i64) -> Result<String, String> {
    let maybe_uuid: Option<String> = with_connection(|conn| {
        let existing: Option<Option<String>> = conn
            .query_row(
                "SELECT uuid FROM clipboard WHERE id = ?1 LIMIT 1",
                params![id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?;

        let existing = existing.flatten();

        if let Some(uuid) = existing.clone().filter(|u| !u.trim().is_empty()) {
            return Ok(Some(uuid));
        }

        let new_uuid = Uuid::new_v4().to_string();
        conn.execute(
            "UPDATE clipboard SET uuid = ?1 WHERE id = ?2 AND (uuid IS NULL OR uuid = '')",
            params![new_uuid, id],
        )?;

        let uuid: Option<Option<String>> = conn
            .query_row(
                "SELECT uuid FROM clipboard WHERE id = ?1 LIMIT 1",
                params![id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?;

        Ok(uuid.flatten())
    })?;

    maybe_uuid
        .filter(|u| !u.trim().is_empty())
        .ok_or_else(|| "生成 uuid 失败".to_string())
}

pub fn get_clipboard_item_id_by_uuid(uuid: &str) -> Result<Option<i64>, String> {
    with_connection(|conn| {
        conn.query_row(
            "SELECT id FROM clipboard WHERE uuid = ?1 LIMIT 1",
            params![uuid],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| e.into())
    })
}

// 根据ID获取剪贴板项（指定截断长度）
pub fn get_clipboard_item_by_id_with_limit(id: i64, max_content_length: Option<usize>) -> Result<Option<ClipboardItem>, String> {
    with_connection(|conn| {
        conn.query_row(
            "SELECT id, uuid, source_device_id, is_remote, content, html_content, content_type, image_id, item_order, is_pinned, paste_count, source_app, source_icon_hash, created_at, updated_at, char_count,
                    (SELECT id FROM favorites WHERE source_clipboard_uuid = clipboard.uuid LIMIT 1) AS favorite_id
             FROM clipboard WHERE id = ?",
            params![id],
            |row| {
                let uuid: Option<String> = row.get(1)?;
                let source_device_id: Option<String> = row.get(2)?;
                let is_remote: i64 = row.get(3)?;
                let content: String = row.get(4)?;
                let html_content: Option<String> = row.get(5)?;
                let content_type: String = row.get(6)?;
                let char_count: Option<i64> = row.get(15)?;
                let final_content = if let Some(max_len) = max_content_length {
                    let is_text_type = is_textual_content_type(&content_type);
                    if is_text_type && content.len() > max_len {
                        truncate_string(content.clone(), max_len)
                    } else {
                        content.clone()
                    }
                } else {
                    content.clone()
                };
                
                // 计算字符数
                let final_char_count = if char_count.is_none() && (content_type.contains("text") || content_type.contains("rich_text")) && !content.is_empty() {
                    Some(content.chars().count() as i64)
                } else {
                    char_count
                };
                
                Ok(ClipboardItem {
                    id: row.get(0)?,
                    uuid,
                    favorite_id: row.get(16)?,
                    source_device_id,
                    is_remote: is_remote != 0,
                    content: final_content,
                    html_content,
                    content_type,
                    image_id: row.get(7)?,
                    item_order: row.get(8)?,
                    is_pinned: row.get::<_, i64>(9)? != 0,
                    paste_count: row.get(10)?,
                    source_app: row.get(11)?,
                    source_icon_hash: row.get(12)?,
                    char_count: final_char_count,
                    created_at: row.get(13)?,
                    updated_at: row.get(14)?,
                })
            }
        )
        .optional()
        .map_err(|e| e.into())
    })
}

pub fn increment_paste_count(id: i64) -> Result<(), String> {
    with_connection(|conn| {
        conn.execute(
            "UPDATE clipboard SET paste_count = paste_count + 1 WHERE id = ?",
            params![id],
        )?;
        Ok(())
    })
}

pub fn increment_paste_counts(ids: &[i64]) -> Result<(), String> {
    if ids.is_empty() {
        return Ok(());
    }

    with_connection(|conn| {
        let tx = conn.unchecked_transaction()?;
        for id in ids {
            tx.execute(
                "UPDATE clipboard SET paste_count = paste_count + 1 WHERE id = ?",
                params![id],
            )?;
        }
        tx.commit()?;
        Ok(())
    })
}

// 批量删除分块大小：每块一个独立短事务，单次持锁时间与块大小成正比
const DELETE_CHUNK_SIZE: usize = 200;

// 在一个短事务里删除一批剪贴板行（先删原始数据，再删历史记录），返回删除行数与该批行的图片 ID
fn delete_clipboard_rows_batch(
    conn: &rusqlite::Connection,
    ids: &[i64],
) -> Result<(usize, HashSet<String>), rusqlite::Error> {
    if ids.is_empty() {
        return Ok((0, HashSet::new()));
    }

    let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let mut images: HashSet<String> = HashSet::new();
    let tx = conn.unchecked_transaction()?;

    {
        let sql = format!("SELECT image_id FROM clipboard WHERE id IN ({})", placeholders);
        let mut stmt = tx.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(ids.iter()), |row| {
            row.get::<_, Option<String>>(0)
        })?;
        for row in rows {
            if let Some(raw) = row? {
                for image_id in split_image_ids(&raw) {
                    images.insert(image_id);
                }
            }
        }
    }

    let data_sql = format!(
        "DELETE FROM clipboard_data WHERE target_kind = 'clipboard' AND target_id IN (
            SELECT CAST(id AS TEXT) FROM clipboard WHERE id IN ({})
        )",
        placeholders
    );
    tx.execute(&data_sql, rusqlite::params_from_iter(ids.iter()))?;

    let rows_sql = format!("DELETE FROM clipboard WHERE id IN ({})", placeholders);
    let deleted = tx.execute(&rows_sql, rusqlite::params_from_iter(ids.iter()))?;

    tx.commit()?;
    Ok((deleted, images))
}

// 取一批“超出上限、应被删除”的历史项 ID（保留顺序与原逻辑一致：置顶优先，其次序号、更新时间）
fn select_overflow_ids(
    conn: &rusqlite::Connection,
    max_count: u64,
    limit: usize,
) -> Result<Vec<i64>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT id FROM clipboard
         ORDER BY is_pinned DESC, item_order DESC, updated_at DESC
         LIMIT ?1 OFFSET ?2",
    )?;
    let rows = stmt.query_map(params![limit as i64, max_count], |row| row.get::<_, i64>(0))?;
    rows.collect()
}

// 取一批“清空范围内的历史项 ID”（只作用于快照 id 之前的行，按 id 从小到大推进）
fn select_clear_ids(
    conn: &rusqlite::Connection,
    max_id: i64,
    limit: usize,
) -> Result<Vec<i64>, rusqlite::Error> {
    let mut stmt = conn.prepare("SELECT id FROM clipboard WHERE id <= ?1 ORDER BY id LIMIT ?2")?;
    let rows = stmt.query_map(params![max_id, limit as i64], |row| row.get::<_, i64>(0))?;
    rows.collect()
}

// 限制剪贴板历史数量（删除超出限制的旧记录）
//
// 删除按 DELETE_CHUNK_SIZE 分块：每块一个短事务、独立持锁，并在块与块之间释放锁，
// 采集线程插入时最多等一块，而不是等整张表的删除 + 引用检查。
pub fn limit_clipboard_history(max_count: u64) -> Result<(), String> {
    if max_count >= 999999 {
        return Ok(());
    }

    // 未超限时直接返回，避免每写入一条都做一次删除/引用检查
    let total: i64 = with_connection(|conn| {
        conn.query_row("SELECT COUNT(*) FROM clipboard", [], |row| row.get(0))
    })?;
    if total <= max_count as i64 {
        return Ok(());
    }

    let mut images_to_delete: HashSet<String> = HashSet::new();
    let mut deleted_total: usize = 0;

    loop {
        let (deleted, batch_images) = with_connection(|conn| {
            let ids = select_overflow_ids(conn, max_count, DELETE_CHUNK_SIZE)?;
            delete_clipboard_rows_batch(conn, &ids)
        })?;

        if deleted == 0 {
            break;
        }
        deleted_total += deleted;
        images_to_delete.extend(batch_images);
    }

    if deleted_total == 0 {
        return Ok(());
    }

    let unreferenced = with_connection(|conn| {
        maybe_shrink_after_bulk_delete(conn, deleted_total);
        collect_unreferenced_image_ids(conn, images_to_delete)
    })?;

    delete_image_files(unreferenced)
}

// 删除单个剪贴板项
pub fn delete_clipboard_item(id: i64) -> Result<(), String> {
    let images_to_delete: Vec<String> = with_connection(|conn| {
        let image_ids: Option<Option<String>> = conn
            .query_row(
                "SELECT image_id FROM clipboard WHERE id = ?",
                params![id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?;
        let Some(image_ids) = image_ids else {
            return Ok(Vec::new());
        };

        conn.execute("DELETE FROM clipboard WHERE id = ?1", params![id])?;

        let mut to_delete = Vec::new();
        if let Some(ids) = image_ids {
            for iid in split_image_ids(&ids) {
                if !is_image_id_referenced(conn, &iid)? {
                    to_delete.push(iid);
                }
            }
        }
        Ok(to_delete)
    })?;

    let _ = delete_clipboard_data_items("clipboard", &id.to_string());
    delete_image_files(images_to_delete)
}

pub fn delete_clipboard_items(ids: &[i64]) -> Result<(), String> {
    if ids.is_empty() {
        return Ok(());
    }

    let unique_ids: Vec<i64> = ids
        .iter()
        .copied()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    // SQLite 绑定变量数量有上限，大范围删除（如全选）按块处理；每块独立短事务、独立持锁
    let mut images_to_delete: HashSet<String> = HashSet::new();
    let mut deleted_total: usize = 0;

    for chunk in unique_ids.chunks(DELETE_CHUNK_SIZE) {
        let (deleted, batch_images) =
            with_connection(|conn| delete_clipboard_rows_batch(conn, chunk))?;
        deleted_total += deleted;
        images_to_delete.extend(batch_images);
    }

    if deleted_total == 0 {
        return Ok(());
    }

    let unreferenced = with_connection(|conn| {
        maybe_shrink_after_bulk_delete(conn, deleted_total);
        collect_unreferenced_image_ids(conn, images_to_delete)
    })?;

    delete_image_files(unreferenced)
}

// 清空所有剪贴板历史
//
// 先快照调用瞬间的最大 id：清空期间的删除只作用于快照之前的行，
// 避免把用户刚复制进来、id 更大的新条目一并删掉。
// 删除同样分块短事务，避免一次长事务独占连接。
pub fn clear_clipboard_history() -> Result<(), String> {
    let max_id: Option<i64> = with_connection(|conn| {
        conn.query_row("SELECT MAX(id) FROM clipboard", [], |row| row.get(0))
    })?;

    let Some(max_id) = max_id else {
        return Ok(());
    };

    let mut images_to_delete: HashSet<String> = HashSet::new();
    let mut deleted_total: usize = 0;

    loop {
        let (deleted, batch_images) = with_connection(|conn| {
            let ids = select_clear_ids(conn, max_id, DELETE_CHUNK_SIZE)?;
            delete_clipboard_rows_batch(conn, &ids)
        })?;

        if deleted == 0 {
            break;
        }
        deleted_total += deleted;
        images_to_delete.extend(batch_images);
    }

    // 兜底：清掉快照范围内可能残留的原始数据（例如历史遗留的孤儿行），
    // 快照之后新采集的条目及其原始数据保持不动
    with_connection(|conn| {
        conn.execute(
            "DELETE FROM clipboard_data
             WHERE target_kind = 'clipboard' AND CAST(target_id AS INTEGER) <= ?1",
            params![max_id],
        )?;
        Ok(())
    })?;

    let unreferenced = with_connection(|conn| {
        maybe_shrink_after_bulk_delete(conn, deleted_total);
        collect_unreferenced_image_ids(conn, images_to_delete)
    })?;

    delete_image_files(unreferenced)
}

// 排序逻辑
fn reorder_items(conn: &rusqlite::Connection, from_idx: usize, to_idx: usize, items: &[(i64, i64)]) -> Result<(), rusqlite::Error> {
    if from_idx == to_idx { return Ok(()); }
    
    let tx = conn.unchecked_transaction()?;
    let now = chrono::Local::now().timestamp();
    let moved_id = items[from_idx].0;
    let target_order = items[to_idx].1;

    if from_idx < to_idx {
        for i in (from_idx + 1)..=to_idx {
            tx.execute("UPDATE clipboard SET item_order = item_order + 1, updated_at = ?1 WHERE id = ?2", params![now, items[i].0])?;
        }
    } else {
        for i in to_idx..from_idx {
            tx.execute("UPDATE clipboard SET item_order = item_order - 1, updated_at = ?1 WHERE id = ?2", params![now, items[i].0])?;
        }
    }
    tx.execute("UPDATE clipboard SET item_order = ?1, updated_at = ?2 WHERE id = ?3", params![target_order, now, moved_id])?;
    tx.commit()
}

// 移动剪贴板项到顶部（非置顶区的顶部）
pub fn move_clipboard_item_to_top(id: i64) -> Result<(), String> {
    with_connection(|conn| {
        let now = chrono::Local::now().timestamp();
        let max_order: i64 = conn.query_row(
            "SELECT COALESCE(MAX(item_order), 0) FROM clipboard WHERE is_pinned = 0",
            [],
            |row| row.get(0)
        ).unwrap_or(0);
        
        conn.execute(
            "UPDATE clipboard SET item_order = ?1, updated_at = ?2 WHERE id = ?3 AND is_pinned = 0",
            params![max_order + 1, now, id],
        )?;
        Ok(())
    })
}

// 移动剪贴板项
pub fn move_clipboard_item_by_id(from_id: i64, to_id: i64) -> Result<(), String> {
    if from_id == to_id { return Ok(()); }

    with_connection(|conn| {
        let from_pinned: i64 = conn.query_row(
            "SELECT is_pinned FROM clipboard WHERE id = ?",
            params![from_id], |row| row.get(0)
        )?;
        let to_pinned: i64 = conn.query_row(
            "SELECT is_pinned FROM clipboard WHERE id = ?",
            params![to_id], |row| row.get(0)
        )?;
        
        if from_pinned != to_pinned {
            return Ok(());
        }
        
        let items: Vec<(i64, i64)> = conn.prepare("SELECT id, item_order FROM clipboard ORDER BY is_pinned DESC, item_order DESC, updated_at DESC")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;

        let from_idx = items.iter().position(|(id, _)| *id == from_id)
            .ok_or_else(|| rusqlite::Error::InvalidParameterName(format!("ID {} 不存在", from_id)))?;
        let to_idx = items.iter().position(|(id, _)| *id == to_id)
            .ok_or_else(|| rusqlite::Error::InvalidParameterName(format!("ID {} 不存在", to_id)))?;
        
        reorder_items(conn, from_idx, to_idx, &items)
    })
}

// 更新剪贴板项的内容
pub fn update_clipboard_item(
    id: i64,
    content: String,
    html_content: Option<String>,
) -> Result<(), String> {
    let should_clear_raw_formats = with_connection(|conn| {
        let (old_content, old_html_content, content_type): (String, Option<String>, String) = conn.query_row(
            "SELECT content, html_content, content_type FROM clipboard WHERE id = ?1",
            params![id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;

        let content_changed = old_content != content;
        let html_changed = html_content
            .as_ref()
            .map(|new_html| old_html_content.as_deref() != Some(new_html.as_str()))
            .unwrap_or(false);

        // 内容变更后同步刷新去重哈希，避免旧哈希导致后续去重失效
        let content_hash = compute_content_hash(&content, &content_type);
        let now = chrono::Local::now().timestamp();
        let rows = if let Some(ref html_content) = html_content {
            conn.execute(
                "UPDATE clipboard SET content = ?1, html_content = ?2, content_hash = ?3, updated_at = ?4 WHERE id = ?5",
                params![&content, html_content, &content_hash, now, id],
            )?
        } else {
            conn.execute(
                "UPDATE clipboard SET content = ?1, content_hash = ?2, updated_at = ?3 WHERE id = ?4",
                params![&content, &content_hash, now, id],
            )?
        };
        if rows == 0 {
            Err(rusqlite::Error::QueryReturnedNoRows)
        } else {
            Ok(content_changed || html_changed)
        }
    }).map_err(|e| if e.contains("QueryReturnedNoRows") {
        format!("剪贴板项不存在: {}", id)
    } else { e })?;

    if should_clear_raw_formats {
        delete_clipboard_data_items("clipboard", &id.to_string())?;
    }

    Ok(())
}

// 切换剪贴板项的置顶状态（置顶时放到置顶区第一位，取消置顶时移到非置顶区第一位）
pub fn toggle_pin_clipboard_item(id: i64) -> Result<bool, String> {
    with_connection(|conn| {
        let current_pinned: i64 = conn.query_row(
            "SELECT is_pinned FROM clipboard WHERE id = ?", params![id], |row| row.get(0)
        )?;
        
        let now = chrono::Local::now().timestamp();
        if current_pinned == 0 {
            let max_pinned_order: i64 = conn.query_row(
                "SELECT COALESCE(MAX(item_order), 0) FROM clipboard WHERE is_pinned = 1", [], |row| row.get(0)
            ).unwrap_or(0);
            conn.execute("UPDATE clipboard SET is_pinned = 1, item_order = ?1, updated_at = ?2 WHERE id = ?3", params![max_pinned_order + 1, now, id])?;
            Ok(true)
        } else {
            let max_order: i64 = conn.query_row(
                "SELECT COALESCE(MAX(item_order), 0) FROM clipboard WHERE is_pinned = 0", [], |row| row.get(0)
            ).unwrap_or(0);
            conn.execute("UPDATE clipboard SET is_pinned = 0, item_order = ?1, updated_at = ?2 WHERE id = ?3", params![max_order + 1, now, id])?;
            Ok(false)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{delete_clipboard_rows_batch, select_clear_ids, select_overflow_ids};
    use rusqlite::Connection;

    // 用内存库验证删除/选行语义，不接触真实数据目录
    fn memory_db() -> Connection {
        let conn = Connection::open_in_memory().expect("打开内存数据库失败");
        conn.execute_batch(
            "CREATE TABLE clipboard (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                image_id TEXT,
                item_order INTEGER NOT NULL DEFAULT 0,
                is_pinned INTEGER NOT NULL DEFAULT 0,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE clipboard_data (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                target_kind TEXT NOT NULL,
                target_id TEXT NOT NULL,
                raw_data BLOB
            );",
        )
        .expect("创建测试表失败");
        conn
    }

    fn insert_row(
        conn: &Connection,
        image_id: Option<&str>,
        item_order: i64,
        is_pinned: i64,
        updated_at: i64,
    ) -> i64 {
        conn.execute(
            "INSERT INTO clipboard (image_id, item_order, is_pinned, updated_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![image_id, item_order, is_pinned, updated_at],
        )
        .expect("插入测试数据失败");
        conn.last_insert_rowid()
    }

    #[test]
    fn select_overflow_ids_prefers_pinned_then_newest() {
        let conn = memory_db();
        let oldest = insert_row(&conn, None, 1, 0, 100);
        let pinned = insert_row(&conn, None, 0, 1, 100);
        let mid = insert_row(&conn, None, 3, 0, 200);
        let newest = insert_row(&conn, None, 2, 0, 300);

        // 4 行保留 2 行：应删除序号最小的 oldest 与非置顶最旧的 newest
        let batch = select_overflow_ids(&conn, 2, 200).expect("查询超限行失败");
        assert_eq!(batch.len(), 2);
        assert!(batch.contains(&oldest));
        assert!(batch.contains(&newest));
        assert!(!batch.contains(&pinned), "置顶行必须保留");
        assert!(!batch.contains(&mid), "保留下来的非置顶行不应被删除");

        // 删除后再次查询不应再返回任何行（分块循环的终止条件）
        let (deleted, _) = delete_clipboard_rows_batch(&conn, &batch).expect("批量删除失败");
        assert_eq!(deleted, 2);
        assert!(select_overflow_ids(&conn, 2, 200).expect("再次查询失败").is_empty());
    }

    #[test]
    fn select_clear_ids_only_returns_rows_before_snapshot() {
        let conn = memory_db();
        let first = insert_row(&conn, None, 1, 0, 100);
        let second = insert_row(&conn, None, 2, 0, 200);
        let third = insert_row(&conn, None, 3, 0, 300);
        let snapshot = third;
        // 快照之后新采集的条目不应被清空
        let later = insert_row(&conn, None, 4, 0, 400);

        let batch = select_clear_ids(&conn, snapshot, 200).expect("查询清空范围失败");
        assert_eq!(batch, vec![first, second, third]);
        assert!(!batch.contains(&later));

        delete_clipboard_rows_batch(&conn, &batch).expect("批量删除失败");
        assert!(select_clear_ids(&conn, snapshot, 200).expect("再次查询失败").is_empty());
        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM clipboard", [], |row| row.get(0))
            .expect("统计剩余行失败");
        assert_eq!(remaining, 1, "快照之外的条目应保留");
    }

    #[test]
    fn delete_batch_removes_rows_data_and_reports_image_ids() {
        let conn = memory_db();
        let first = insert_row(&conn, Some("img-a,img-b"), 1, 0, 100);
        let second = insert_row(&conn, None, 2, 0, 200);
        let third = insert_row(&conn, Some("img-c"), 3, 0, 300);
        for id in [first, second, third] {
            conn.execute(
                "INSERT INTO clipboard_data (target_kind, target_id, raw_data)
                 VALUES ('clipboard', ?1, x'00')",
                rusqlite::params![id.to_string()],
            )
            .expect("插入原始数据失败");
        }

        let (deleted, images) =
            delete_clipboard_rows_batch(&conn, &[first, second]).expect("批量删除失败");
        assert_eq!(deleted, 2);
        assert_eq!(images.len(), 2);
        assert!(images.contains("img-a"));
        assert!(images.contains("img-b"));

        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM clipboard", [], |row| row.get(0))
            .expect("统计剩余行失败");
        assert_eq!(rows, 1);
        let data_rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM clipboard_data", [], |row| row.get(0))
            .expect("统计剩余原始数据失败");
        assert_eq!(data_rows, 1, "只应删除被删行对应的原始数据");
    }
}

