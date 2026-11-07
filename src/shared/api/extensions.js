import { invoke } from '@tauri-apps/api/core'

/**
 * 安装扩展。path 可以是 .zip 安装包，也可以是一个扩展目录。
 * 返回 { status: 'installed' | 'conflict', id, name, overwritten }；
 * status 为 conflict 时磁盘未被修改，需要用户确认后带 overwrite=true 重试。
 */
export async function installExtension(path, overwrite = false) {
  return await invoke('install_extension', { path, overwrite })
}

export async function uninstallExtension(id) {
  return await invoke('uninstall_extension', { id })
}
