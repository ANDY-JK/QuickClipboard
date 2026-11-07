//! 扩展安装 / 卸载。
//!
//! 安装来源有两种：单个 `.zip` 安装包，或一个现成的扩展目录。
//! 统一流程是「先落到临时目录 → 在临时目录上校验 → 原子 rename 成正式目录」，
//! 任一步失败都会清理临时目录，磁盘上不会留下半个扩展。
//!
//! 临时目录放在 `extensions/` 的父目录（同一卷）而不是系统临时目录：
//! 跨卷 rename 会失败；放进 `extensions/` 内部又会被 [`super::list_extensions`]
//! 当成一个非法扩展列出来。

use super::{
    extensions_dir, is_valid_extension_id, read_extension_dir, read_manifest, MANIFEST_FILE_NAME,
};
use serde::Serialize;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

/// 安装包条目数上限（扩展通常只有 manifest 加少量 JS）
const MAX_ARCHIVE_ENTRIES: usize = 200;
/// 安装包内单个文件上限
const MAX_ARCHIVE_ENTRY_BYTES: u64 = 2 * 1024 * 1024;
/// 安装包解包后的总大小上限
const MAX_ARCHIVE_TOTAL_BYTES: u64 = 8 * 1024 * 1024;

/// 安装结果。`status` 为 `conflict` 时表示目标 id 已存在，前端应询问用户是否覆盖。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallOutcome {
    pub status: String,
    pub id: String,
    pub name: String,
    pub overwritten: bool,
}

impl InstallOutcome {
    fn conflict(id: &str, name: &str) -> Self {
        Self {
            status: "conflict".to_string(),
            id: id.to_string(),
            name: name.to_string(),
            overwritten: false,
        }
    }

    fn installed(id: &str, name: &str, overwritten: bool) -> Self {
        Self {
            status: "installed".to_string(),
            id: id.to_string(),
            name: name.to_string(),
            overwritten,
        }
    }
}

/// 安装扩展。`source` 可以是 `.zip` 文件，也可以是一个扩展目录。
///
/// `overwrite=false` 且目标 id 已存在时返回 `status == "conflict"`，
/// 不修改磁盘；用户确认后前端带 `overwrite=true` 再调一次。
pub fn install_extension(source: &Path, overwrite: bool) -> Result<InstallOutcome, String> {
    let root = extensions_dir()?;
    let staging_parent = root
        .parent()
        .ok_or_else(|| "扩展目录路径异常".to_string())?
        .to_path_buf();
    install_from_source(source, &root, &staging_parent, overwrite)
}

/// 删除扩展目录。调用方应先把对应的 Worker dispose 掉，避免文件被占用。
pub fn uninstall_extension(id: &str) -> Result<(), String> {
    if !is_valid_extension_id(id) {
        return Err("非法扩展 id".to_string());
    }

    let dir = extensions_dir()?.join(id);
    if !dir.is_dir() {
        return Err("扩展未安装".to_string());
    }

    fs::remove_dir_all(&dir).map_err(|e| format!("删除扩展目录失败: {}", e))
}

/// 安装主体。四个路径参数全部显式传入，便于单测在临时目录里跑完整流程。
fn install_from_source(
    source: &Path,
    extensions_root: &Path,
    staging_parent: &Path,
    overwrite: bool,
) -> Result<InstallOutcome, String> {
    let staging = create_staging_dir(staging_parent)?;
    let result = stage_validate_publish(source, &staging, extensions_root, staging_parent, overwrite);
    // 成功发布时 staging 已被 rename 掉，这里的清理对它是 no-op；
    // 失败或只需返回冲突时清掉解包残留，保证「不留半个扩展目录」。
    if staging.exists() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

fn stage_validate_publish(
    source: &Path,
    staging: &Path,
    extensions_root: &Path,
    staging_parent: &Path,
    overwrite: bool,
) -> Result<InstallOutcome, String> {
    if source.is_dir() {
        copy_source_dir(source, staging)?;
    } else if source.is_file() {
        if !has_zip_extension(source) {
            return Err("安装包必须是 .zip 文件或一个扩展目录".to_string());
        }
        extract_zip(source, staging)?;
    } else {
        return Err("安装来源不存在".to_string());
    }

    // 校验一：manifest 位置（包根或唯一一层子目录）
    let candidate = locate_manifest_dir(staging)?;
    // 校验二：清单本身（存在性/体积/可解析）
    let manifest = read_manifest(&candidate)?;

    if !is_valid_extension_id(&manifest.id) {
        return Err("manifest id 不符合命名规范".to_string());
    }

    // 校验三：嵌套布局时，子目录名必须与 manifest id 一致，
    // 否则用户会拿到「目录是 foo、id 是 bar」这种无法自洽的安装结果。
    if candidate != staging {
        let sub_name = candidate
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| "安装子目录名不可用".to_string())?
            .to_string();
        if sub_name != manifest.id {
            return Err(format!(
                "manifest id 与目录名不一致：id 为「{}」，目录为「{}」",
                manifest.id, sub_name
            ));
        }
    }

    // 校验四：复用扫描时的那套逻辑（id、name、version、entry 解析与越界检查）
    let info = read_extension_dir(&candidate, &manifest.id);
    if !info.valid {
        return Err(info
            .error
            .unwrap_or_else(|| "扩展未通过校验".to_string()));
    }

    let target = extensions_root.join(&manifest.id);
    if target.exists() && !overwrite {
        return Ok(InstallOutcome::conflict(&manifest.id, &info.name));
    }

    publish(&candidate, &target, staging_parent)?;
    Ok(InstallOutcome::installed(&manifest.id, &info.name, overwrite))
}

/// `extensions/<id>` 已存在时：先把它挪到同卷备份，再 rename 新目录进去；
/// 新目录 rename 失败就把备份挪回来。成功后再删备份。
fn publish(candidate: &Path, target: &Path, staging_parent: &Path) -> Result<(), String> {
    let backup = if target.exists() {
        let path = staging_parent.join(format!(".ext-backup-{}", unique_suffix()));
        fs::rename(target, &path).map_err(|e| format!("备份旧扩展目录失败: {}", e))?;
        Some(path)
    } else {
        None
    };

    match fs::rename(candidate, target) {
        Ok(()) => {
            if let Some(path) = backup {
                let _ = fs::remove_dir_all(&path);
            }
            Ok(())
        }
        Err(error) => {
            if let Some(path) = backup {
                let _ = fs::rename(&path, target);
            }
            Err(format!("写入扩展目录失败: {}", error))
        }
    }
}

fn create_staging_dir(parent: &Path) -> Result<PathBuf, String> {
    fs::create_dir_all(parent).map_err(|e| format!("创建扩展目录失败: {}", e))?;
    let dir = parent.join(format!(".ext-install-{}", unique_suffix()));
    fs::create_dir(&dir).map_err(|e| format!("创建临时安装目录失败: {}", e))?;
    Ok(dir)
}

fn unique_suffix() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{}", std::process::id(), nanos)
}

fn has_zip_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("zip"))
        .unwrap_or(false)
}

/// 定位 manifest 所在目录：包根，或唯一一层子目录。
fn locate_manifest_dir(staging: &Path) -> Result<PathBuf, String> {
    if staging.join(MANIFEST_FILE_NAME).is_file() {
        return Ok(staging.to_path_buf());
    }

    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut other: Vec<PathBuf> = Vec::new();
    for entry in fs::read_dir(staging)
        .map_err(|e| format!("读取临时安装目录失败: {}", e))?
        .flatten()
    {
        let path = entry.path();
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            dirs.push(path);
        } else {
            other.push(path);
        }
    }

    if let [dir] = dirs.as_slice() {
        if other.is_empty() && dir.join(MANIFEST_FILE_NAME).is_file() {
            return Ok(dir.clone());
        }
    }

    Err("manifest.json 必须位于安装包根目录或唯一一层子目录下".to_string())
}

/// 校验 zip 条目路径，返回可直接拼接到目标目录的相对路径。
///
/// 拒绝：空路径、NUL、含 `:`（盘符/Windows 数据流）、绝对路径、根路径、
/// `..` 上跳、UNC（`\\server\share`）、以及任何非 Normal 组件。
fn sanitize_entry_path(raw: &str) -> Result<PathBuf, String> {
    if raw.is_empty() {
        return Err("安装包包含空路径条目".to_string());
    }
    if raw.contains('\0') {
        return Err("安装包条目包含 NUL 字符".to_string());
    }
    if raw.contains(':') {
        return Err(format!("安装包条目包含非法字符: {}", raw));
    }

    // zip 规范用 `/`，但部分打包工具会写 `\`；统一成 `/` 后再按组件判定，
    // 避免 Windows 上把 `..\evil.js` 当成一个普通文件名。
    let normalized = raw.replace('\\', "/");
    if normalized.starts_with('/') {
        return Err(format!("安装包条目是绝对路径: {}", raw));
    }

    for component in Path::new(&normalized).components() {
        match component {
            Component::Normal(_) => {}
            _ => return Err(format!("安装包条目包含非法路径: {}", raw)),
        }
    }

    Ok(PathBuf::from(normalized))
}

/// 解包前逐条校验（路径、条目数、声明体积），解包时再按实际字节数复核。
fn extract_zip(zip_path: &Path, dst: &Path) -> Result<(), String> {
    let file = fs::File::open(zip_path).map_err(|e| format!("打开安装包失败: {}", e))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!("读取安装包失败: {}", e))?;

    if archive.len() > MAX_ARCHIVE_ENTRIES {
        return Err(format!(
            "安装包条目数超过上限 {}（实际 {}）",
            MAX_ARCHIVE_ENTRIES,
            archive.len()
        ));
    }

    // 第一遍：只读元数据，把非法路径与超限条目全部找出来，一个字节都不落盘。
    let mut declared_total: u64 = 0;
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|e| format!("读取安装包条目失败: {}", e))?;
        sanitize_entry_path(&entry.name().to_string())?;

        let size = entry.size();
        if size > MAX_ARCHIVE_ENTRY_BYTES {
            return Err(format!(
                "安装包内文件超过 {} KB 上限: {}",
                MAX_ARCHIVE_ENTRY_BYTES / 1024,
                entry.name()
            ));
        }
        declared_total += size;
        if declared_total > MAX_ARCHIVE_TOTAL_BYTES {
            return Err(format!(
                "安装包解包后总大小超过 {} KB 上限",
                MAX_ARCHIVE_TOTAL_BYTES / 1024
            ));
        }
    }

    // 第二遍：真正写盘，按实际读出的字节数复核（central directory 里的 size 可以伪造）。
    let mut written_total: u64 = 0;
    let mut written_entries: usize = 0;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|e| format!("读取安装包条目失败: {}", e))?;
        let rel = sanitize_entry_path(&entry.name().to_string())?;
        let out_path = dst.join(&rel);

        written_entries += 1;
        if written_entries > MAX_ARCHIVE_ENTRIES {
            return Err(format!("安装包条目数超过上限 {}", MAX_ARCHIVE_ENTRIES));
        }

        if entry.is_dir() {
            fs::create_dir_all(&out_path)
                .map_err(|e| format!("创建安装目录失败: {}", e))?;
            continue;
        }

        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("创建安装目录失败: {}", e))?;
        }

        let mut buffer = Vec::new();
        entry
            .by_ref()
            .take(MAX_ARCHIVE_ENTRY_BYTES + 1)
            .read_to_end(&mut buffer)
            .map_err(|e| format!("解包条目失败: {}", e))?;
        if buffer.len() as u64 > MAX_ARCHIVE_ENTRY_BYTES {
            return Err(format!(
                "安装包内文件超过 {} KB 上限: {}",
                MAX_ARCHIVE_ENTRY_BYTES / 1024,
                entry.name()
            ));
        }

        written_total += buffer.len() as u64;
        if written_total > MAX_ARCHIVE_TOTAL_BYTES {
            return Err(format!(
                "安装包解包后总大小超过 {} KB 上限",
                MAX_ARCHIVE_TOTAL_BYTES / 1024
            ));
        }

        fs::write(&out_path, &buffer).map_err(|e| format!("写入安装文件失败: {}", e))?;
    }

    Ok(())
}

#[derive(Default)]
struct CopyStats {
    entries: usize,
    total_bytes: u64,
}

/// 把现成的扩展目录复制到临时目录，沿用与 zip 相同的三类上限。
fn copy_source_dir(src: &Path, dst: &Path) -> Result<(), String> {
    copy_dir_recursive(src, dst, &mut CopyStats::default())
}

fn copy_dir_recursive(src: &Path, dst: &Path, stats: &mut CopyStats) -> Result<(), String> {
    for entry in fs::read_dir(src).map_err(|e| format!("读取安装来源失败: {}", e))?.flatten() {
        let name = entry
            .file_name()
            .to_str()
            .map(|name| name.to_string())
            .ok_or_else(|| "安装来源包含无法识别的文件名".to_string())?;

        // 目录来源虽然是本机路径，仍按 zip 同一套规则拒绝可疑名字，
        // 避免 `a:b` 这类 Windows 数据流路径落到扩展目录里。
        sanitize_entry_path(&name)?;

        let from = entry.path();
        let metadata =
            fs::symlink_metadata(&from).map_err(|e| format!("读取安装来源失败: {}", e))?;

        // 符号链接可能指到扩展目录之外，直接拒绝，不做「跟随」。
        if metadata.file_type().is_symlink() {
            return Err(format!("安装来源包含符号链接，已拒绝: {}", name));
        }

        stats.entries += 1;
        if stats.entries > MAX_ARCHIVE_ENTRIES {
            return Err(format!("安装来源条目数超过上限 {}", MAX_ARCHIVE_ENTRIES));
        }

        let to = dst.join(&name);
        if metadata.is_dir() {
            fs::create_dir_all(&to).map_err(|e| format!("创建安装目录失败: {}", e))?;
            copy_dir_recursive(&from, &to, stats)?;
        } else if metadata.is_file() {
            if metadata.len() > MAX_ARCHIVE_ENTRY_BYTES {
                return Err(format!(
                    "安装来源内文件超过 {} KB 上限: {}",
                    MAX_ARCHIVE_ENTRY_BYTES / 1024,
                    name
                ));
            }
            let copied =
                fs::copy(&from, &to).map_err(|e| format!("复制安装文件失败: {}", e))?;
            if copied > MAX_ARCHIVE_ENTRY_BYTES {
                return Err(format!(
                    "安装来源内文件超过 {} KB 上限: {}",
                    MAX_ARCHIVE_ENTRY_BYTES / 1024,
                    name
                ));
            }
            stats.total_bytes += copied;
            if stats.total_bytes > MAX_ARCHIVE_TOTAL_BYTES {
                return Err(format!(
                    "安装来源解包后总大小超过 {} KB 上限",
                    MAX_ARCHIVE_TOTAL_BYTES / 1024
                ));
            }
        } else {
            return Err(format!("安装来源包含非常规文件: {}", name));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!("qc-ext-test-{}-{}", tag, unique_suffix()));
            fs::create_dir_all(&path).expect("创建测试目录");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write_extension(dir: &Path, manifest_id: &str) {
        fs::create_dir_all(dir).expect("创建扩展目录");
        fs::write(
            dir.join("manifest.json"),
            format!(
                r#"{{"id":"{}","name":"测试扩展","version":"1.0.0","entry":"index.js"}}"#,
                manifest_id
            ),
        )
        .expect("写 manifest");
        fs::write(dir.join("index.js"), "export const extension = { id: 'x', identify: () => [] }\n")
            .expect("写入口");
    }

    fn build_zip(path: &Path, entries: &[(&str, &str)]) {
        let file = fs::File::create(path).expect("创建 zip");
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        for (name, content) in entries {
            writer.start_file(*name, options).expect("写入条目");
            use std::io::Write;
            writer.write_all(content.as_bytes()).expect("写入内容");
        }
        writer.finish().expect("完成 zip");
    }

    #[test]
    fn rejects_zip_slip_paths() {
        assert!(sanitize_entry_path("../evil.js").is_err());
        assert!(sanitize_entry_path("pkg/../../evil.js").is_err());
        assert!(sanitize_entry_path("/etc/passwd").is_err());
        assert!(sanitize_entry_path(r"C:\Windows\evil.js").is_err());
        assert!(sanitize_entry_path(r"\\server\share\evil.js").is_err());
        assert!(sanitize_entry_path(r"..\evil.js").is_err());
        assert!(sanitize_entry_path("pkg/index.js").is_ok());
    }

    #[test]
    fn rejects_archive_over_limits() {
        let temp = TempDir::new("bomb");
        let zip_path = temp.path().join("bomb.zip");
        // 条目数上限 200：造 201 个空文件，解包前就应中止。
        let mut writer = zip::ZipWriter::new(fs::File::create(&zip_path).expect("创建 zip"));
        let options = zip::write::SimpleFileOptions::default();
        use std::io::Write;
        for index in 0..(MAX_ARCHIVE_ENTRIES + 1) {
            writer
                .start_file(format!("f{}.js", index), options)
                .expect("写入条目");
            writer.write_all(b"x").expect("写入内容");
        }
        writer.finish().expect("完成 zip");

        let staging = temp.path().join("staging");
        fs::create_dir_all(&staging).expect("创建 staging");
        let error = extract_zip(&zip_path, &staging).expect_err("超限包必须被拒绝");
        assert!(error.contains("条目数超过上限"), "实际错误: {}", error);
    }

    #[test]
    fn rejects_invalid_manifest() {
        let temp = TempDir::new("bad-manifest");
        let source = temp.path().join("source");
        fs::create_dir_all(&source).expect("创建来源");
        fs::write(source.join("manifest.json"), "{ not json").expect("写坏 manifest");
        fs::write(source.join("index.js"), "").expect("写入口");

        let staging = temp.path().join("staging");
        fs::create_dir_all(&staging).expect("创建 staging");
        let error = stage_validate_publish(&source, &staging, &temp.path().join("ext"), temp.path(), false)
            .expect_err("非法 manifest 必须被拒绝");
        assert!(error.contains("manifest.json 解析失败"), "实际错误: {}", error);
    }

    #[test]
    fn rejects_id_mismatched_directory() {
        let temp = TempDir::new("id-mismatch");
        let zip_path = temp.path().join("pkg.zip");
        build_zip(
            &zip_path,
            &[
                (
                    "wrongid/manifest.json",
                    r#"{"id":"another","name":"测试扩展","version":"1.0.0","entry":"index.js"}"#,
                ),
                ("wrongid/index.js", "export const extension = {}"),
            ],
        );

        let extensions = temp.path().join("ext");
        fs::create_dir_all(&extensions).expect("创建扩展根");
        let error = install_from_source(&zip_path, &extensions, temp.path(), false)
            .expect_err("id 与目录名不一致必须被拒绝");
        assert!(error.contains("manifest id 与目录名不一致"), "实际错误: {}", error);
        assert!(!extensions.join("another").exists(), "失败的安装不得留下目录");
    }

    #[test]
    fn installs_directory_source_and_reports_conflict() {
        let temp = TempDir::new("install-dir");
        let source = temp.path().join("myext");
        write_extension(&source, "myext");

        let extensions = temp.path().join("ext");
        fs::create_dir_all(&extensions).expect("创建扩展根");

        let outcome =
            install_from_source(&source, &extensions, temp.path(), false).expect("首次安装成功");
        assert_eq!(outcome.status, "installed");
        assert!(extensions.join("myext").join("manifest.json").is_file());

        let again = install_from_source(&source, &extensions, temp.path(), false)
            .expect("重复安装应返回冲突而不是报错");
        assert_eq!(again.status, "conflict");

        let overwritten = install_from_source(&source, &extensions, temp.path(), true)
            .expect("覆盖安装成功");
        assert_eq!(overwritten.status, "installed");
        assert!(overwritten.overwritten);
    }
}
