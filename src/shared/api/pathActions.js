import { invoke } from '@tauri-apps/api/core'

/**
 * 在系统文件管理器中打开应用数据目录。
 * kind 仅支持：storage | extensions | clipboard_images | pin_images | image_library | thumbnails
 * 路径由后端解析，前端不传具体路径。
 */
export async function openAppDataFolder(kind) {
  return await invoke('open_app_data_folder', { kind })
}

//用系统默认程序打开剪贴板中的本地文件（后端会拦截可执行/脚本类文件）
export async function openLocalFile(path) {
  return await invoke('open_local_file', { path })
}

//在系统文件管理器中定位本地文件/目录
export async function revealLocalPath(path) {
  return await invoke('reveal_local_path', { path })
}
