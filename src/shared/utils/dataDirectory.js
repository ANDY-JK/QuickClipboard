import { invoke } from '@tauri-apps/api/core'

// 数据目录是进程级常量，列表每行挂载都 invoke 一次会造成大量重复 IPC。
// 使用模块级单例 Promise，失败时清空以便下次重试。
let dataDirectoryPromise = null

export function getDataDirectory() {
  if (!dataDirectoryPromise) {
    dataDirectoryPromise = invoke('get_data_directory').catch((error) => {
      dataDirectoryPromise = null
      throw error
    })
  }
  return dataDirectoryPromise
}
