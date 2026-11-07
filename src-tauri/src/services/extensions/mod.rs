//! 扩展发现与元数据校验（MVP）
//!
//! 扩展位于应用数据目录下的 `extensions/<id>/`，每个扩展包含 `manifest.json`
//! 与入口 JS 文件。本模块只做磁盘扫描与严格校验，不执行扩展代码。
//! 安装/卸载见子模块 [`install`]。

pub mod install;

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

/// manifest.json 体积上限
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
/// 入口源码体积上限
const MAX_ENTRY_BYTES: u64 = 512 * 1024;
/// 单次扫描的扩展数量上限
const MAX_EXTENSIONS: usize = 200;
/// 扩展 id 最大长度（首字符 + 1..=63）
const MAX_ID_CHARS: usize = 64;

const EXTENSIONS_DIR_NAME: &str = "extensions";
const MANIFEST_FILE_NAME: &str = "manifest.json";

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ExtensionActivation {
    pub content_types: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ExtensionManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub entry: String,
    pub description: String,
    pub activation: ExtensionActivation,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub content_types: Vec<String>,
    // 仅内部用于定位入口文件，不下发给前端：避免把本地绝对路径暴露到 WebView。
    #[serde(skip_serializing)]
    pub entry_path: String,
    pub valid: bool,
    pub error: Option<String>,
}

/// 扩展 id 规范：`^[a-z0-9][a-z0-9._-]{1,63}$`
pub fn is_valid_extension_id(id: &str) -> bool {
    if id.is_empty() || id.chars().count() > MAX_ID_CHARS {
        return false;
    }

    let mut chars = id.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }

    if id.chars().count() < 2 {
        return false;
    }

    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

pub fn extensions_dir() -> Result<PathBuf, String> {
    let dir = crate::services::get_data_directory()?.join(EXTENSIONS_DIR_NAME);
    fs::create_dir_all(&dir).map_err(|e| format!("创建扩展目录失败: {}", e))?;
    Ok(dir)
}

/// 扫描扩展目录。非法扩展仍然返回，但标记 valid=false 并给出原因。
pub fn list_extensions() -> Result<Vec<ExtensionInfo>, String> {
    let dir = extensions_dir()?;
    let mut dir_entries: Vec<PathBuf> = Vec::new();

    let read_dir = fs::read_dir(&dir).map_err(|e| format!("读取扩展目录失败: {}", e))?;
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.is_dir() {
            dir_entries.push(path);
        }
    }
    dir_entries.sort();

    let mut result = Vec::new();
    for (index, path) in dir_entries.iter().enumerate() {
        let dir_name = match path.file_name().and_then(|name| name.to_str()) {
            Some(name) => name.to_string(),
            None => continue,
        };

        if index >= MAX_EXTENSIONS {
            result.push(ExtensionInfo {
                id: dir_name,
                name: String::new(),
                version: String::new(),
                description: String::new(),
                content_types: Vec::new(),
                entry_path: String::new(),
                valid: false,
                error: Some(format!("扩展数量超过上限 {}", MAX_EXTENSIONS)),
            });
            continue;
        }

        result.push(read_extension_dir(path, &dir_name));
    }

    Ok(result)
}

/// 读取已通过校验的扩展入口源码。
pub fn read_extension_entry(id: &str) -> Result<String, String> {
    if !is_valid_extension_id(id) {
        return Err("非法扩展 id".to_string());
    }

    let dir = extensions_dir()?;
    let info = read_extension_dir(&dir.join(id), id);
    if !info.valid {
        return Err(info.error.unwrap_or_else(|| "扩展未通过校验".to_string()));
    }

    let entry_path = PathBuf::from(&info.entry_path);
    // 直接以流式限读代替「先 metadata 再 read」：文件在两次系统调用之间被替换时，
    // 读取量仍然受 MAX_ENTRY_BYTES 约束，不会绕过上限。
    let file = fs::File::open(&entry_path).map_err(|e| format!("读取入口文件失败: {}", e))?;
    let mut bytes = Vec::new();
    file.take(MAX_ENTRY_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("读取入口文件失败: {}", e))?;
    if bytes.len() as u64 > MAX_ENTRY_BYTES {
        return Err(format!(
            "入口文件超过 {} KB 上限",
            MAX_ENTRY_BYTES / 1024
        ));
    }

    String::from_utf8(bytes).map_err(|_| "入口文件不是 UTF-8 文本".to_string())
}

fn read_extension_dir(dir: &Path, dir_name: &str) -> ExtensionInfo {
    let mut info = ExtensionInfo {
        id: dir_name.to_string(),
        name: String::new(),
        version: String::new(),
        description: String::new(),
        content_types: Vec::new(),
        entry_path: String::new(),
        valid: false,
        error: None,
    };

    if !is_valid_extension_id(dir_name) {
        info.error = Some("目录名不是合法扩展 id".to_string());
        return info;
    }

    let manifest = match read_manifest(dir) {
        Ok(manifest) => manifest,
        Err(error) => {
            info.error = Some(error);
            return info;
        }
    };

    // 先把可展示字段回填，便于前端在非法状态下也能显示基本信息。
    // id 始终使用目录名：非法项不得冒用 manifest 里声明的别家 id，
    // 否则列表会出现重复 id（也会造成 React key 冲突）。
    info.name = manifest.name.clone();
    info.version = manifest.version.clone();
    info.description = manifest.description.clone();
    info.content_types = manifest.activation.content_types.clone();

    if manifest.id != dir_name {
        info.error = Some("manifest id 与目录名不一致".to_string());
        return info;
    }

    if !is_valid_extension_id(&manifest.id) {
        info.error = Some("manifest id 不符合命名规范".to_string());
        return info;
    }

    if manifest.name.trim().is_empty() {
        info.error = Some("manifest 缺少 name".to_string());
        return info;
    }

    if manifest.version.trim().is_empty() {
        info.error = Some("manifest 缺少 version".to_string());
        return info;
    }

    match resolve_entry_path(dir, &manifest.entry) {
        Ok(path) => {
            // 对外只暴露常规路径：`canonicalize` 会带上 `\\?\` 扩展长度前缀，
            // 直接展示/复制会显得怪异，也影响前端按路径打开。
            // 安全校验仍然基于规范化路径完成，这里仅做展示层去前缀。
            info.entry_path = display_path(&path);
            info.valid = true;
            info
        }
        Err(error) => {
            info.error = Some(error);
            info
        }
    }
}

/// 读取并解析 manifest.json，按「存在性 → 是否文件 → 体积 → 可读 → 可解析」逐层校验。
///
/// 安装流程在解包后也调用这里，保证安装与扫描使用同一套清单校验口径。
/// 返回的错误文案与 [`read_extension_dir`] 中展示给用户的文案一致。
fn read_manifest(dir: &Path) -> Result<ExtensionManifest, String> {
    let manifest_path = dir.join(MANIFEST_FILE_NAME);
    let metadata =
        fs::metadata(&manifest_path).map_err(|_| "缺少 manifest.json".to_string())?;

    if !metadata.is_file() {
        return Err("manifest.json 不是文件".to_string());
    }

    if metadata.len() > MAX_MANIFEST_BYTES {
        return Err(format!(
            "manifest.json 超过 {} KB 上限",
            MAX_MANIFEST_BYTES / 1024
        ));
    }

    let manifest_text =
        fs::read_to_string(&manifest_path).map_err(|e| format!("读取 manifest.json 失败: {}", e))?;

    serde_json::from_str(&manifest_text).map_err(|e| format!("manifest.json 解析失败: {}", e))
}

/// 去掉 Windows `\\?\` 扩展长度前缀，返回常规显示路径。
fn display_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{}", rest);
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return rest.to_string();
    }
    text.to_string()
}

fn resolve_entry_path(dir: &Path, entry: &str) -> Result<PathBuf, String> {
    let entry = entry.trim();
    if entry.is_empty() {
        return Err("manifest 缺少 entry".to_string());
    }

    if !entry.ends_with(".js") {
        return Err("entry 必须以 .js 结尾".to_string());
    }

    let entry_path = Path::new(entry);
    if entry_path.is_absolute() {
        return Err("entry 必须是相对路径".to_string());
    }

    for component in entry_path.components() {
        match component {
            Component::Normal(_) => {}
            _ => return Err("entry 不能包含 .. 或根路径".to_string()),
        }
    }

    let joined = dir.join(entry_path);
    let canonical_dir = fs::canonicalize(dir).map_err(|e| format!("扩展目录不可用: {}", e))?;
    let canonical_entry =
        fs::canonicalize(&joined).map_err(|_| format!("入口文件不存在: {}", entry))?;

    if !canonical_entry.starts_with(&canonical_dir) {
        return Err("entry 解析后超出扩展目录".to_string());
    }

    if !canonical_entry.is_file() {
        return Err("entry 不是文件".to_string());
    }

    Ok(canonical_entry)
}

#[cfg(test)]
mod tests {
    use super::{display_path, is_valid_extension_id};
    use std::path::Path;

    #[test]
    fn strips_extended_length_prefix() {
        assert_eq!(
            display_path(Path::new(r"\\?\C:\Users\q\extensions\dev.hello\index.js")),
            r"C:\Users\q\extensions\dev.hello\index.js"
        );
        assert_eq!(
            display_path(Path::new(r"\\?\UNC\server\share\index.js")),
            r"\\server\share\index.js"
        );
        assert_eq!(
            display_path(Path::new(r"C:\plain\index.js")),
            r"C:\plain\index.js"
        );
    }

    #[test]
    fn extension_id_format() {
        assert!(is_valid_extension_id("dev.hello"));
        assert!(is_valid_extension_id("a1"));
        assert!(!is_valid_extension_id("A"));
        assert!(!is_valid_extension_id("UPPER"));
        assert!(!is_valid_extension_id(".hidden"));
        assert!(!is_valid_extension_id("bad/name"));
        assert!(!is_valid_extension_id("bad\\name"));
        assert!(!is_valid_extension_id(""));
    }
}
