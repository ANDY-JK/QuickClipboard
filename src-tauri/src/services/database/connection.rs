use rusqlite::{Connection, params};
use parking_lot::Mutex;
use once_cell::sync::Lazy;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub const MAX_CONTENT_LENGTH: usize = 1600;

// 数据库连接
static DB_CONNECTION: Lazy<Mutex<Option<Connection>>> = 
    Lazy::new(|| Mutex::new(None));

// 维护模式：压缩数据库期间拒绝新的数据库操作，避免替换文件时仍有连接持有句柄
static MAINTENANCE_MODE: AtomicBool = AtomicBool::new(false);

pub fn set_maintenance_mode(active: bool) {
    MAINTENANCE_MODE.store(active, Ordering::SeqCst);
}

pub fn is_maintenance_mode() -> bool {
    MAINTENANCE_MODE.load(Ordering::SeqCst)
}

// 初始化数据库连接
pub fn init_database(db_path: &str) -> Result<(), String> {
    let conn = Connection::open(db_path)
        .map_err(|e| format!("打开数据库失败: {}", e))?;

    // 写入遇到其他连接持锁时最多等待 5s，避免长事务期间直接报 database is locked
    conn.busy_timeout(Duration::from_secs(5))
        .map_err(|e| format!("设置数据库超时失败: {}", e))?;

    // 新库启用增量收缩；老库已建表时该 PRAGMA 不生效，需一次 VACUUM（重操作，不自动执行）
    let _ = conn.execute_batch("PRAGMA auto_vacuum = INCREMENTAL;");

    // 创建表结构
    create_tables(&conn)?;

    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA cache_size = 10000;
         PRAGMA temp_store = MEMORY;"
    ).map_err(|e| format!("设置数据库参数失败: {}", e))?;

    log_auto_vacuum_mode(&conn);

    // 一次性迁移收藏为全局序号（使用 user_version 控制，仅执行一次）
    migrate_favorites_global_order_if_needed(&conn)?;

    // 一次性回填 content_hash（user_version 门控，可中断续跑）
    migrate_clipboard_content_hash_if_needed(&conn)?;

    // 一次性重排剪贴板序号 + 清理收藏自动标题（user_version: 2 -> 3）
    migrate_order_and_favorite_titles_if_needed(&conn)?;
    
    let mut db_conn = DB_CONNECTION.lock();
    *db_conn = Some(conn);
    
    Ok(())
}

// 关闭数据库连接
pub fn close_database() {
    let mut db_conn = DB_CONNECTION.lock();
    if db_conn.is_some() {
        *db_conn = None;
    }
}

// 记录当前 auto_vacuum 模式；老库为 0 时增量收缩无效，只能靠一次 VACUUM 开启（不自动执行）
fn log_auto_vacuum_mode(conn: &Connection) {
    match conn.query_row("PRAGMA auto_vacuum", [], |row| row.get::<_, i64>(0)) {
        Ok(2) => {}
        Ok(mode) => eprintln!(
            "[db] auto_vacuum={}（非 INCREMENTAL），批量删除后不会收缩；开启需一次 VACUUM，暂不自动执行",
            mode
        ),
        Err(e) => eprintln!("[db] 读取 auto_vacuum 状态失败: {}", e),
    }
}

// 删除至少这么多行后才尝试回收空闲页，避免小删除频繁触发
const INCREMENTAL_VACUUM_MIN_ROWS: usize = 100;

// 删除大量数据后尝试回收空闲页；只有 auto_vacuum=INCREMENTAL 的库才会真正收缩
//
// 这里只做一次轻量的模式读取并调度后台收缩，不在当前调用方（采集线程或命令线程）里
// 执行真空。原因是 PRAGMA incremental_vacuum 会一次性回收全部空闲页，属于长操作；
// 在采集链路上执行会让后续复制内容排队等锁，表现为“复制后历史里没有新条目”。
pub fn maybe_shrink_after_bulk_delete(conn: &Connection, deleted_rows: usize) {
    if deleted_rows < INCREMENTAL_VACUUM_MIN_ROWS {
        return;
    }

    let mode: i64 = conn
        .query_row("PRAGMA auto_vacuum", [], |row| row.get(0))
        .unwrap_or(0);
    if mode != 2 {
        eprintln!(
            "[db] 删除 {} 行后跳过收缩（auto_vacuum={}），文件体积暂不回收",
            deleted_rows, mode
        );
        return;
    }

    schedule_incremental_vacuum();
}

// 后台增量收缩：单 worker、请求合并，并且把真空拆成有限页数的小步。
//
// 锁策略取舍：仍然复用全局单写连接 + 同一把 Mutex（不新增连接、不新增读池），
// 这样所有写入在进程内串行，不存在“我方两个连接互相写冲突”的问题；同时也避免了
// 额外连接长期持有数据库文件句柄，干扰 storage_maintenance 的压缩替换流程
// （Windows 下打开中的文件无法改名/替换）。
// 代价是真空的每一步会短暂阻塞其它 DB 访问，所以用 VACUUM_PAGE_STEP 限制单步页数，
// 步间 sleep 让采集写入有机会插入，采集线程最多等一步而不是等整轮真空。
const VACUUM_PAGE_STEP: i64 = 200;
const VACUUM_STEP_INTERVAL: Duration = Duration::from_millis(15);

struct VacuumTask {
    running: bool,
    pending: bool,
}

static VACUUM_TASK: Lazy<Mutex<VacuumTask>> = Lazy::new(|| {
    Mutex::new(VacuumTask {
        running: false,
        pending: false,
    })
});

// 调度一次后台增量收缩。重复调用会合并到同一个 worker，不会叠加执行。
pub fn schedule_incremental_vacuum() {
    {
        let mut task = VACUUM_TASK.lock();
        task.pending = true;
        if task.running {
            return;
        }
        // running 与 pending 必须在同一把锁里翻转，避免“置位后 worker 已退出”的漏唤醒
        task.running = true;
    }

    let spawned = std::thread::Builder::new()
        .name("db-incremental-vacuum".to_string())
        .spawn(run_incremental_vacuum_worker);

    if let Err(e) = spawned {
        eprintln!("[db] 启动后台收缩线程失败: {}", e);
        let mut task = VACUUM_TASK.lock();
        task.running = false;
        task.pending = false;
    }
}

fn run_incremental_vacuum_worker() {
    loop {
        // running 与 pending 在同一把锁里判断，保证新请求不会在退出瞬间丢失
        {
            let mut task = VACUUM_TASK.lock();
            if !task.pending {
                task.running = false;
                return;
            }
            task.pending = false;
        }

        let mut last_free_pages: i64 = -1;
        loop {
            match vacuum_step() {
                Ok(Some(remaining)) => {
                    // 没有空闲页或本步没有进展（非增量模式的异常情况）就结束本轮
                    if remaining <= 0 || remaining == last_free_pages {
                        break;
                    }
                    last_free_pages = remaining;
                    std::thread::sleep(VACUUM_STEP_INTERVAL);
                }
                Ok(None) => break,
                Err(e) => {
                    eprintln!("[db] 后台增量收缩中止: {}", e);
                    break;
                }
            }
        }
    }
}

// 执行一步真空，返回回收后剩余的空闲页数；None 表示无需或无法收缩。
fn vacuum_step() -> Result<Option<i64>, String> {
    with_connection(|conn| {
        let mode: i64 = conn
            .query_row("PRAGMA auto_vacuum", [], |row| row.get(0))
            .unwrap_or(0);
        if mode != 2 {
            return Ok(None);
        }

        let free_pages: i64 = conn.query_row("PRAGMA freelist_count", [], |row| row.get(0))?;
        if free_pages <= 0 {
            return Ok(None);
        }

        // 有参数的 incremental_vacuum 每次最多回收 N 页，锁持有时间与 N 成正比
        conn.execute_batch(&format!("PRAGMA incremental_vacuum({});", VACUUM_PAGE_STEP))?;
        let remaining: i64 = conn.query_row("PRAGMA freelist_count", [], |row| row.get(0))?;
        Ok(Some(remaining))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::sync::Mutex;

    // init_database 操作的是进程级全局连接，并行执行会互相覆盖，测试之间串行化
    static DB_GLOBAL_LOCK: Mutex<()> = Mutex::new(());

    fn temp_db_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "qc_migration_test_{}_{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("创建临时目录失败");
        dir
    }

    // 全新空库首次启动：完整跑一遍迁移链（0 -> 1 -> 2 -> 3），不能 panic/报错
    #[test]
    fn fresh_empty_database_runs_full_migration_chain() {
        let _guard = DB_GLOBAL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = temp_db_dir("fresh");
        let db_path = dir.join("quickclipboard.db");
        let db_path_str = db_path.to_str().expect("临时数据库路径不是合法 UTF-8");

        // 修复前这里会因空表 MAX(item_order)=NULL 读到 InvalidColumnType 而失败
        init_database(db_path_str).expect("全新空库首次启动不应失败");

        let (version, clipboard_rows, favorite_rows, group_rows): (i32, i64, i64, i64) =
            with_connection(|conn| {
                Ok((
                    conn.query_row("PRAGMA user_version;", [], |row| row.get(0))?,
                    conn.query_row("SELECT COUNT(*) FROM clipboard", [], |row| row.get(0))?,
                    conn.query_row("SELECT COUNT(*) FROM favorites", [], |row| row.get(0))?,
                    conn.query_row("SELECT COUNT(*) FROM groups", [], |row| row.get(0))?,
                ))
            })
            .expect("读取空库迁移后状态失败");

        assert_eq!(version, 3, "全新空库应完成全部迁移并落到 user_version=3");
        assert_eq!(clipboard_rows, 0, "空库不应被迁移写入数据");
        assert_eq!(favorite_rows, 0, "空库不应被迁移写入数据");
        assert_eq!(group_rows, 0, "空库不应被迁移写入数据");

        // 迁移后仍可正常写入：新条目使用默认序号，不依赖迁移结果
        with_connection(|conn| {
            conn.execute(
                "INSERT INTO clipboard (content, content_type, created_at, updated_at)
                 VALUES ('空库首条', 'text', 1, 1)",
                [],
            )?;
            Ok(())
        })
        .expect("空库迁移后写入失败");

        let order: i64 = with_connection(|conn| {
            conn.query_row(
                "SELECT item_order FROM clipboard ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
        })
        .expect("读取新插入条目序号失败");
        assert_eq!(order, 0);

        close_database();
        let _ = std::fs::remove_dir_all(&dir);
    }

    // 老库（user_version=0、旧 ASC 序号、收藏自动标题）升级到当前版本
    #[test]
    fn legacy_database_upgrade_runs_all_migrations() {
        let _guard = DB_GLOBAL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = temp_db_dir("legacy");
        let db_path = dir.join("quickclipboard.db");

        {
            let conn = Connection::open(&db_path).expect("创建模拟老库失败");
            conn.execute_batch(
                "CREATE TABLE clipboard (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    content TEXT NOT NULL,
                    html_content TEXT,
                    content_type TEXT NOT NULL DEFAULT 'text',
                    image_id TEXT,
                    item_order INTEGER NOT NULL DEFAULT 0,
                    is_pinned INTEGER NOT NULL DEFAULT 0,
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL
                 );
                 CREATE TABLE favorites (
                    id TEXT PRIMARY KEY,
                    title TEXT NOT NULL,
                    content TEXT NOT NULL,
                    html_content TEXT,
                    content_type TEXT NOT NULL DEFAULT 'text',
                    image_id TEXT,
                    group_name TEXT NOT NULL DEFAULT '全部',
                    item_order INTEGER NOT NULL DEFAULT 0,
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL
                 );
                 INSERT INTO clipboard (content, content_type, item_order, created_at, updated_at)
                    VALUES ('最旧', 'text', 0, 100, 100),
                           ('中间', 'text', 0, 100, 200),
                           ('最新', 'text', 0, 100, 300);
                 INSERT INTO favorites (id, title, content, content_type, item_order, created_at, updated_at)
                    VALUES ('fav-file', 'file:///C:/temp/a.txt', 'file:///C:/temp/a.txt', 'file', 0, 100, 100),
                           ('fav-note', '手写标题', '手写标题', 'text', 0, 100, 200);",
            )
            .expect("写入模拟老库数据失败");
        }

        let db_path_str = db_path.to_str().expect("临时数据库路径不是合法 UTF-8");
        init_database(db_path_str).expect("老库升级迁移不应失败");

        let version: i32 = with_connection(|conn| {
            conn.query_row("PRAGMA user_version;", [], |row| row.get(0))
        })
        .expect("读取升级后 user_version 失败");
        assert_eq!(version, 3, "老库应升级到 user_version=3");

        // 迁移 3：旧 ASC 序号按 updated_at DESC 重排为 DESC（最新=3）
        let (newest_order, middle_order, oldest_order): (i64, i64, i64) =
            with_connection(|conn| {
                Ok((
                    conn.query_row("SELECT item_order FROM clipboard WHERE id = 3", [], |row| row.get(0))?,
                    conn.query_row("SELECT item_order FROM clipboard WHERE id = 2", [], |row| row.get(0))?,
                    conn.query_row("SELECT item_order FROM clipboard WHERE id = 1", [], |row| row.get(0))?,
                ))
            })
            .expect("读取迁移后序号失败");
        assert_eq!((newest_order, middle_order, oldest_order), (3, 2, 1));

        // 迁移 2：content_hash 全部回填（非 NULL 且非空串哨兵以外的有效值）
        let null_hashes: i64 = with_connection(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM clipboard WHERE content_hash IS NULL OR content_hash = ''",
                [],
                |row| row.get(0),
            )
        })
        .expect("统计 content_hash 回填结果失败");
        assert_eq!(null_hashes, 0, "老库 content_hash 应全部回填");

        // 迁移 3：file 类型收藏的自动标题应清空，手写标题保持不变
        let (file_title, note_title): (String, String) = with_connection(|conn| {
            Ok((
                conn.query_row("SELECT title FROM favorites WHERE id = 'fav-file'", [], |row| row.get(0))?,
                conn.query_row("SELECT title FROM favorites WHERE id = 'fav-note'", [], |row| row.get(0))?,
            ))
        })
        .expect("读取迁移后收藏标题失败");
        assert_eq!(file_title, "", "file 收藏的自动标题应被清空");
        assert_eq!(note_title, "手写标题", "非 file/image 收藏标题不应被改动");

        // 迁移 1：收藏序号按全局重排（updated_at 更新的 fav-note 拿到 2）
        let (note_order, file_order): (i64, i64) = with_connection(|conn| {
            Ok((
                conn.query_row("SELECT item_order FROM favorites WHERE id = 'fav-note'", [], |row| row.get(0))?,
                conn.query_row("SELECT item_order FROM favorites WHERE id = 'fav-file'", [], |row| row.get(0))?,
            ))
        })
        .expect("读取迁移后收藏序号失败");
        assert_eq!((note_order, file_order), (2, 1));

        close_database();
        let _ = std::fs::remove_dir_all(&dir);
    }

    // 验证“带页数上限的 incremental_vacuum”确实能回收空闲页（后台分步收缩依赖该语法）
    #[test]
    fn bounded_incremental_vacuum_reclaims_pages() {
        let dir = std::env::temp_dir().join(format!("qc_vacuum_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("创建临时目录失败");
        let db_path = dir.join("vacuum.db");

        {
            let conn = Connection::open(&db_path).expect("打开临时数据库失败");
            conn.execute_batch("PRAGMA auto_vacuum = INCREMENTAL;").expect("设置 auto_vacuum 失败");
            conn.execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY, blob BLOB);")
                .expect("创建测试表失败");

            let mode: i64 = conn
                .query_row("PRAGMA auto_vacuum", [], |row| row.get(0))
                .expect("读取 auto_vacuum 失败");
            assert_eq!(mode, 2, "临时库应处于 INCREMENTAL 模式");

            let payload = vec![7u8; 4096];
            let tx = conn.unchecked_transaction().expect("开启事务失败");
            for _ in 0..200 {
                tx.execute("INSERT INTO t (blob) VALUES (?1)", rusqlite::params![payload])
                    .expect("插入测试数据失败");
            }
            tx.commit().expect("提交事务失败");
            conn.execute("DELETE FROM t", []).expect("删除测试数据失败");

            let before: i64 = conn
                .query_row("PRAGMA freelist_count", [], |row| row.get(0))
                .expect("读取空闲页失败");
            assert!(before > 0, "删除后应产生空闲页");

            conn.execute_batch("PRAGMA incremental_vacuum(200);")
                .expect("带页数上限的增量真空失败");
            let after: i64 = conn
                .query_row("PRAGMA freelist_count", [], |row| row.get(0))
                .expect("读取空闲页失败");
            assert!(
                after < before,
                "有页数上限的增量真空应回收部分空闲页: {} -> {}",
                before,
                after
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}

// WAL 收敛：把 -wal 中已提交事务并回主库；仍失败时由调用方决定是否备份重建
pub fn checkpoint_wal_truncate(db_path: &str) -> Result<(), String> {
    let conn = Connection::open(db_path).map_err(|e| format!("打开数据库失败: {}", e))?;
    let _ = conn.busy_timeout(Duration::from_secs(5));
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .map_err(|e| format!("WAL checkpoint 失败: {}", e))
}

// 数据库无法打开时把 db/-wal/-shm 改名备份（保留全部字节，不删除），返回备份文件列表
pub fn backup_corrupt_database(db_path: &str) -> Result<Vec<String>, String> {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let mut backups = Vec::new();
    let mut partial_error: Option<String> = None;

    for suffix in ["", "-wal", "-shm"] {
        let src = format!("{}{}", db_path, suffix);
        if !Path::new(&src).exists() {
            continue;
        }

        let dst = format!("{}.corrupt-{}.bak", src, stamp);
        match std::fs::rename(&src, &dst) {
            Ok(()) => backups.push(dst),
            Err(e) => {
                // 主库备份失败则不能再重建，直接返回；wal/shm 失败只记录，主库仍可重建
                if suffix.is_empty() {
                    return Err(format!("备份数据库失败: {} -> {}: {}", src, dst, e));
                }
                partial_error = Some(format!("备份 {} 失败: {}", src, e));
            }
        }
    }

    if let Some(err) = partial_error {
        eprintln!("[db] {}", err);
    }
    Ok(backups)
}

// 创建数据库表
fn create_tables(conn: &Connection) -> Result<(), String> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS clipboard (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            content TEXT NOT NULL,
            html_content TEXT,
            content_type TEXT NOT NULL DEFAULT 'text',
            image_id TEXT,
            item_order INTEGER NOT NULL DEFAULT 0,
            is_pinned INTEGER NOT NULL DEFAULT 0,
            uuid TEXT,
            source_device_id TEXT,
            is_remote INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        )",
        [],
    ).map_err(|e| format!("创建剪贴板表失败: {}", e))?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS clipboard_data (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            target_kind TEXT NOT NULL,
            target_id TEXT NOT NULL,
            format_name TEXT NOT NULL,
            raw_data BLOB NOT NULL,
            is_primary INTEGER NOT NULL DEFAULT 0,
            format_order INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        )",
        [],
    ).map_err(|e| format!("创建剪贴板原始数据表失败: {}", e))?;

    let pinned_exists = conn
        .prepare("PRAGMA table_info(clipboard)")
        .and_then(|mut stmt| {
            let columns = stmt.query_map([], |row| Ok(row.get::<_, String>(1)?))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(columns.iter().any(|c| c == "is_pinned"))
        })
        .unwrap_or(false);
    
    if !pinned_exists {
        conn.execute(
            "ALTER TABLE clipboard ADD COLUMN is_pinned INTEGER NOT NULL DEFAULT 0",
            [],
        ).map_err(|e| format!("添加置顶字段失败: {}", e))?;
    }

    let paste_count_exists = conn
        .prepare("PRAGMA table_info(clipboard)")
        .and_then(|mut stmt| {
            let columns = stmt.query_map([], |row| Ok(row.get::<_, String>(1)?))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(columns.iter().any(|c| c == "paste_count"))
        })
        .unwrap_or(false);
    
    if !paste_count_exists {
        conn.execute(
            "ALTER TABLE clipboard ADD COLUMN paste_count INTEGER NOT NULL DEFAULT 0",
            [],
        ).map_err(|e| format!("添加粘贴次数字段失败: {}", e))?;
    }

    conn.execute(
        "CREATE TABLE IF NOT EXISTS favorites (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            content TEXT NOT NULL,
            html_content TEXT,
            content_type TEXT NOT NULL DEFAULT 'text',
            image_id TEXT,
            source_clipboard_uuid TEXT,
            group_name TEXT NOT NULL DEFAULT '全部',
            item_order INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        )",
        [],
    ).map_err(|e| format!("创建收藏表失败: {}", e))?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS groups (
            name TEXT PRIMARY KEY,
            icon TEXT NOT NULL DEFAULT 'ti ti-folder',
            color TEXT NOT NULL DEFAULT '#dc2626',
            order_index INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        )",
        [],
    ).map_err(|e| format!("创建分组表失败: {}", e))?;

    let color_exists = conn
        .prepare("PRAGMA table_info(groups)")
        .and_then(|mut stmt| {
            let columns = stmt.query_map([], |row| {
                Ok(row.get::<_, String>(1)?)
            })?;
            Ok(columns.into_iter().any(|col| col.map(|c| c == "color").unwrap_or(false)))
        })
        .unwrap_or(false);
    
    if !color_exists {
        conn.execute(
            "ALTER TABLE groups ADD COLUMN color TEXT NOT NULL DEFAULT '#dc2626'",
            [],
        ).map_err(|e| format!("添加颜色字段失败: {}", e))?;
    }

    let fav_paste_count_exists = conn
        .prepare("PRAGMA table_info(favorites)")
        .and_then(|mut stmt| {
            let columns = stmt.query_map([], |row| Ok(row.get::<_, String>(1)?))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(columns.iter().any(|c| c == "paste_count"))
        })
        .unwrap_or(false);
    
    if !fav_paste_count_exists {
        conn.execute(
            "ALTER TABLE favorites ADD COLUMN paste_count INTEGER NOT NULL DEFAULT 0",
            [],
        ).map_err(|e| format!("添加收藏粘贴次数字段失败: {}", e))?;
    }

    let source_app_exists = conn
        .prepare("PRAGMA table_info(clipboard)")
        .and_then(|mut stmt| {
            let columns = stmt.query_map([], |row| Ok(row.get::<_, String>(1)?))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(columns.iter().any(|c| c == "source_app"))
        })
        .unwrap_or(false);
    
    if !source_app_exists {
        conn.execute("ALTER TABLE clipboard ADD COLUMN source_app TEXT", [])
            .map_err(|e| format!("添加来源应用字段失败: {}", e))?;
    }

    let source_icon_hash_exists = conn
        .prepare("PRAGMA table_info(clipboard)")
        .and_then(|mut stmt| {
            let columns = stmt.query_map([], |row| Ok(row.get::<_, String>(1)?))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(columns.iter().any(|c| c == "source_icon_hash"))
        })
        .unwrap_or(false);
    
    if !source_icon_hash_exists {
        conn.execute("ALTER TABLE clipboard ADD COLUMN source_icon_hash TEXT", [])
            .map_err(|e| format!("添加来源图标哈希字段失败: {}", e))?;
    }

    let clip_char_count_exists = conn
        .prepare("PRAGMA table_info(clipboard)")
        .and_then(|mut stmt| {
            let columns = stmt.query_map([], |row| Ok(row.get::<_, String>(1)?))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(columns.iter().any(|c| c == "char_count"))
        })
        .unwrap_or(false);
    
    if !clip_char_count_exists {
        conn.execute("ALTER TABLE clipboard ADD COLUMN char_count INTEGER", [])
            .map_err(|e| format!("添加剪贴板字符数量字段失败: {}", e))?;
    }

    let clip_uuid_exists = conn
        .prepare("PRAGMA table_info(clipboard)")
        .and_then(|mut stmt| {
            let columns = stmt.query_map([], |row| Ok(row.get::<_, String>(1)?))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(columns.iter().any(|c| c == "uuid"))
        })
        .unwrap_or(false);

    if !clip_uuid_exists {
        conn.execute("ALTER TABLE clipboard ADD COLUMN uuid TEXT", [])
            .map_err(|e| format!("添加剪贴板 UUID 字段失败: {}", e))?;
    }

    let clip_source_device_id_exists = conn
        .prepare("PRAGMA table_info(clipboard)")
        .and_then(|mut stmt| {
            let columns = stmt.query_map([], |row| Ok(row.get::<_, String>(1)?))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(columns.iter().any(|c| c == "source_device_id"))
        })
        .unwrap_or(false);

    if !clip_source_device_id_exists {
        conn.execute("ALTER TABLE clipboard ADD COLUMN source_device_id TEXT", [])
            .map_err(|e| format!("添加剪贴板来源设备字段失败: {}", e))?;
    }

    let clip_is_remote_exists = conn
        .prepare("PRAGMA table_info(clipboard)")
        .and_then(|mut stmt| {
            let columns = stmt.query_map([], |row| Ok(row.get::<_, String>(1)?))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(columns.iter().any(|c| c == "is_remote"))
        })
        .unwrap_or(false);

    if !clip_is_remote_exists {
        conn.execute("ALTER TABLE clipboard ADD COLUMN is_remote INTEGER NOT NULL DEFAULT 0", [])
            .map_err(|e| format!("添加剪贴板远端标记字段失败: {}", e))?;
    }

    let clip_content_hash_exists = conn
        .prepare("PRAGMA table_info(clipboard)")
        .and_then(|mut stmt| {
            let columns = stmt.query_map([], |row| Ok(row.get::<_, String>(1)?))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(columns.iter().any(|c| c == "content_hash"))
        })
        .unwrap_or(false);

    if !clip_content_hash_exists {
        conn.execute("ALTER TABLE clipboard ADD COLUMN content_hash TEXT", [])
            .map_err(|e| format!("添加剪贴板内容哈希字段失败: {}", e))?;
    }

    let fav_char_count_exists = conn
        .prepare("PRAGMA table_info(favorites)")
        .and_then(|mut stmt| {
            let columns = stmt.query_map([], |row| Ok(row.get::<_, String>(1)?))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(columns.iter().any(|c| c == "char_count"))
        })
        .unwrap_or(false);
    
    if !fav_char_count_exists {
        conn.execute("ALTER TABLE favorites ADD COLUMN char_count INTEGER", [])
            .map_err(|e| format!("添加收藏字符数量字段失败: {}", e))?;
    }

    let fav_source_device_id_exists = conn
        .prepare("PRAGMA table_info(favorites)")
        .and_then(|mut stmt| {
            let columns = stmt.query_map([], |row| Ok(row.get::<_, String>(1)?))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(columns.iter().any(|c| c == "source_device_id"))
        })
        .unwrap_or(false);

    if !fav_source_device_id_exists {
        conn.execute("ALTER TABLE favorites ADD COLUMN source_device_id TEXT", [])
            .map_err(|e| format!("添加收藏来源设备字段失败: {}", e))?;
    }

    let fav_source_clipboard_uuid_exists = conn
        .prepare("PRAGMA table_info(favorites)")
        .and_then(|mut stmt| {
            let columns = stmt.query_map([], |row| Ok(row.get::<_, String>(1)?))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(columns.iter().any(|c| c == "source_clipboard_uuid"))
        })
        .unwrap_or(false);

    if !fav_source_clipboard_uuid_exists {
        conn.execute("ALTER TABLE favorites ADD COLUMN source_clipboard_uuid TEXT", [])
            .map_err(|e| format!("添加收藏来源历史字段失败: {}", e))?;
    }

    let group_source_device_id_exists = conn
        .prepare("PRAGMA table_info(groups)")
        .and_then(|mut stmt| {
            let columns = stmt.query_map([], |row| Ok(row.get::<_, String>(1)?))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(columns.iter().any(|c| c == "source_device_id"))
        })
        .unwrap_or(false);

    if !group_source_device_id_exists {
        conn.execute("ALTER TABLE groups ADD COLUMN source_device_id TEXT", [])
            .map_err(|e| format!("添加分组来源设备字段失败: {}", e))?;
    }

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_clipboard_order ON clipboard(is_pinned DESC, item_order DESC, updated_at DESC)",
        [],
    ).map_err(|e| format!("创建剪贴板排序索引失败: {}", e))?;
    
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_clipboard_content_type ON clipboard(content_type)",
        [],
    ).map_err(|e| format!("创建内容类型索引失败: {}", e))?;

    conn.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_clipboard_data_unique
         ON clipboard_data(target_kind, target_id, format_name)",
        [],
    ).map_err(|e| format!("创建剪贴板原始数据唯一索引失败: {}", e))?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_clipboard_data_target_order
         ON clipboard_data(target_kind, target_id, format_order, id)",
        [],
    ).map_err(|e| format!("创建剪贴板原始数据索引失败: {}", e))?;

    conn.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_clipboard_uuid_unique ON clipboard(uuid) WHERE uuid IS NOT NULL AND uuid <> ''",
        [],
    ).map_err(|e| format!("创建剪贴板 UUID 唯一索引失败: {}", e))?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_favorites_group ON favorites(group_name, item_order)",
        [],
    ).map_err(|e| format!("创建收藏索引失败: {}", e))?;

    conn.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_favorites_source_clipboard_uuid
         ON favorites(source_clipboard_uuid)
         WHERE source_clipboard_uuid IS NOT NULL AND source_clipboard_uuid <> ''",
        [],
    ).map_err(|e| format!("创建收藏来源历史唯一索引失败: {}", e))?;

    // 去重精确查询：content_hash 等值查（非唯一，允许历史数据哈希碰撞）
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_clipboard_content_hash ON clipboard(content_hash)",
        [],
    ).map_err(|e| format!("创建剪贴板内容哈希索引失败: {}", e))?;

    // 历史回收/引用检查：按 updated_at 取尾部、按 image_id 做覆盖扫描
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_clipboard_updated_at_id ON clipboard(updated_at, id)",
        [],
    ).map_err(|e| format!("创建剪贴板更新时间索引失败: {}", e))?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_clipboard_image_id ON clipboard(image_id)",
        [],
    ).map_err(|e| format!("创建剪贴板图片索引失败: {}", e))?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_favorites_image_id ON favorites(image_id)",
        [],
    ).map_err(|e| format!("创建收藏图片索引失败: {}", e))?;

    // 收藏默认排序（不按分组过滤时走该索引）
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_favorites_order ON favorites(item_order DESC, updated_at DESC)",
        [],
    ).map_err(|e| format!("创建收藏排序索引失败: {}", e))?;

    Ok(())
}

// 一次性迁移剪贴板序号 + 清理收藏自动标题（user_version: 2 -> 3）
//
// 这两项此前在 create_tables 中每次启动都会执行：
// - migrate_clipboard_order 的判据可能反复命中，且逐行 UPDATE 无事务；
// - migrate_favorites_auto_titles 会把所有 file/image 收藏的完整 content 读进内存。
// 现在统一由 user_version 门控，只在首次升级时执行一次，且整体包在单事务里。
fn migrate_order_and_favorite_titles_if_needed(conn: &Connection) -> Result<(), String> {
    let current_version: i32 = conn
        .query_row("PRAGMA user_version;", [], |row| row.get(0))
        .map_err(|e| format!("读取 user_version 失败: {}", e))?;

    if current_version >= 3 {
        return Ok(());
    }

    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("启动序号/标题迁移事务失败: {}", e))?;

    migrate_clipboard_order(&tx)?;
    migrate_favorites_auto_titles(&tx)?;

    // user_version 与迁移数据放在同一事务里，避免崩溃后出现「数据已迁移但版本未更新」
    tx.execute("PRAGMA user_version = 3;", [])
        .map_err(|e| format!("更新 user_version 失败: {}", e))?;

    tx.commit()
        .map_err(|e| format!("提交序号/标题迁移事务失败: {}", e))?;

    Ok(())
}

// 迁移 item_order（ASC → DESC），仅在 user_version < 3 时由调用方执行一次
fn migrate_clipboard_order(conn: &Connection) -> Result<(), String> {
    // 空表时 MAX(item_order) 返回 NULL，直接参与比较会让整个表达式变成 NULL，
    // 再按 bool 读取就会报 InvalidColumnType（全新空库首次启动即在此失败），故用 COALESCE 兜底。
    let need_migrate: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM clipboard WHERE item_order < 0) 
         OR (SELECT COALESCE(MAX(item_order), 0) FROM clipboard) < (SELECT COUNT(*) FROM clipboard)",
        [], |row| row.get(0)
    ).map_err(|e| format!("检查剪贴板序号迁移条件失败: {}", e))?;

    if !need_migrate {
        return Ok(());
    }

    let ids: Vec<i64> = {
        let mut stmt = conn
            .prepare(
                "SELECT id FROM clipboard ORDER BY is_pinned DESC, item_order ASC, updated_at DESC",
            )
            .map_err(|e| format!("准备剪贴板序号迁移查询失败: {}", e))?;

        let rows = stmt
            .query_map([], |row| row.get(0))
            .map_err(|e| format!("查询剪贴板序号迁移数据失败: {}", e))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("读取剪贴板序号迁移数据失败: {}", e))?
    };

    let count = ids.len() as i64;
    for (i, id) in ids.iter().enumerate() {
        conn.execute(
            "UPDATE clipboard SET item_order = ?1 WHERE id = ?2",
            rusqlite::params![count - i as i64, id],
        )
        .map_err(|e| format!("更新剪贴板序号失败: {}", e))?;
    }

    Ok(())
}

// 一次性迁移收藏为全局序号
fn migrate_favorites_global_order_if_needed(conn: &Connection) -> Result<(), String> {
    let current_version: i32 = conn
        .query_row("PRAGMA user_version;", [], |row| row.get(0))
        .unwrap_or(0);

    if current_version >= 1 {
        return Ok(());
    }

    let favorites_table_exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'favorites'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    if favorites_table_exists == 0 {
        let _ = conn.execute("PRAGMA user_version = 1;", []);
        return Ok(());
    }

    let favorites_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM favorites", [], |row| row.get(0))
        .unwrap_or(0);
    if favorites_count == 0 {
        let _ = conn.execute("PRAGMA user_version = 1;", []);
        return Ok(());
    }

    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("启动收藏序号迁移事务失败: {}", e))?;

    let ids: Vec<String> = {
        let mut stmt = tx
            .prepare(
                "SELECT id FROM favorites 
                 ORDER BY item_order DESC, updated_at DESC, created_at DESC, id ASC",
            )
            .map_err(|e| format!("准备收藏序号迁移查询失败: {}", e))?;

        let ids: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .map_err(|e| format!("查询收藏序号迁移数据失败: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        ids
    };

    let total = ids.len() as i64;
    for (idx, id) in ids.iter().enumerate() {
        let new_order = total - idx as i64;
        tx.execute(
            "UPDATE favorites SET item_order = ?1 WHERE id = ?2",
            params![new_order, id],
        )
        .ok();
    }

    tx.commit()
        .map_err(|e| format!("提交收藏序号迁移事务失败: {}", e))?;

    conn.execute("PRAGMA user_version = 1;", [])
        .map_err(|e| format!("更新 user_version 失败: {}", e))?;

    Ok(())
}

// 一次性回填 clipboard.content_hash（user_version: 1 -> 2）
//
// 行为:
// - 老库首次启动: 增列后按 id 顺序分批（每批 100 行）读取 content 计算哈希并写回，
//   每批单独提交；全部完成后才把 user_version 置为 2。
// - 中途失败（磁盘/数据库异常）: 当前批次回滚、前面批次保留，函数返回 Err 并向上冒泡
//   （init_database 失败 -> setup 失败 -> 启动中止），不会带着 NULL 哈希继续运行；
//   下次启动从 "content_hash IS NULL" 的位置续跑，已回填的行不会重复计算。
// - 不可能去重的类型写入空串哨兵（非 NULL），既保证循环能推进，也不会与有效哈希匹配。
fn migrate_clipboard_content_hash_if_needed(conn: &Connection) -> Result<(), String> {
    const BATCH_SIZE: i64 = 100;

    let current_version: i32 = conn
        .query_row("PRAGMA user_version;", [], |row| row.get(0))
        .map_err(|e| format!("读取 user_version 失败: {}", e))?;

    if current_version >= 2 {
        return Ok(());
    }

    let clipboard_table_exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'clipboard'",
            [],
            |row| row.get(0),
        )
        .map_err(|e| format!("检查 clipboard 表失败: {}", e))?;

    if clipboard_table_exists == 0 {
        conn.execute("PRAGMA user_version = 2;", [])
            .map_err(|e| format!("更新 user_version 失败: {}", e))?;
        return Ok(());
    }

    loop {
        // 每批只取固定行数，避免历史大字段一次性读入内存
        let batch: Vec<(i64, String, String)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT id, content, content_type FROM clipboard
                     WHERE content_hash IS NULL
                     ORDER BY id ASC
                     LIMIT ?1",
                )
                .map_err(|e| format!("准备 content_hash 回填查询失败: {}", e))?;

            let rows = stmt
                .query_map(params![BATCH_SIZE], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
                })
                .map_err(|e| format!("查询 content_hash 回填数据失败: {}", e))?;

            let batch = rows
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("读取 content_hash 回填批次失败: {}", e))?;

            batch
        };

        if batch.is_empty() {
            break;
        }

        let tx = conn
            .unchecked_transaction()
            .map_err(|e| format!("启动 content_hash 回填事务失败: {}", e))?;

        for (id, content, content_type) in &batch {
            let hash = super::clipboard::compute_content_hash(content, content_type);
            tx.execute(
                "UPDATE clipboard SET content_hash = ?1 WHERE id = ?2",
                params![hash, id],
            )
            .map_err(|e| format!("回填 id={} 的 content_hash 失败: {}", id, e))?;
        }

        tx.commit()
            .map_err(|e| format!("提交 content_hash 回填事务失败: {}", e))?;
    }

    conn.execute("PRAGMA user_version = 2;", [])
        .map_err(|e| format!("更新 user_version 失败: {}", e))?;

    Ok(())
}

// 获取数据库连接
pub fn with_connection<F, R>(f: F) -> Result<R, String>
where
    F: FnOnce(&Connection) -> Result<R, rusqlite::Error>,
{
    if is_maintenance_mode() {
        return Err("数据库正在维护（压缩中），请稍后重试".to_string());
    }

    let conn_guard = DB_CONNECTION.lock();
    let conn = conn_guard.as_ref()
        .ok_or("数据库未初始化")?;
    f(conn).map_err(|e| format!("数据库操作失败: {}", e))
}


// 清理文件和图片类型收藏项的自动生成标题
//
// 仅在 user_version < 3 时执行一次。先只取 id 列表，再逐行读取 title/content，
// 避免把所有收藏的完整 content 一次性读进内存（大文件收藏可能非常多）。
fn migrate_favorites_auto_titles(conn: &Connection) -> Result<(), String> {
    let ids: Vec<String> = {
        let mut stmt = conn
            .prepare(
                "SELECT id FROM favorites WHERE content_type LIKE '%file%' OR content_type LIKE '%image%'",
            )
            .map_err(|e| format!("准备收藏标题迁移查询失败: {}", e))?;

        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| format!("查询收藏标题迁移数据失败: {}", e))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("读取收藏标题迁移数据失败: {}", e))?
    };

    for id in ids {
        let (title, content): (String, String) = conn
            .query_row(
                "SELECT title, content FROM favorites WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| format!("读取收藏 {} 的标题失败: {}", id, e))?;

        let content_chars: Vec<char> = content.chars().collect();
        let expected_title = if content_chars.len() > 50 {
            format!("{}...", content_chars[..50].iter().collect::<String>())
        } else {
            content_chars.iter().collect::<String>()
        };

        if title == expected_title {
            conn.execute("UPDATE favorites SET title = '' WHERE id = ?1", params![id])
                .map_err(|e| format!("清理收藏 {} 的自动标题失败: {}", id, e))?;
        }
    }

    Ok(())
}
