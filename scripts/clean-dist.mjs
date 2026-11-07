import { chmodSync, existsSync, readdirSync, rmdirSync, statSync, unlinkSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

// dist 位于项目根目录（vite root 之外），构建前主动清理，避免旧产物被打进安装包。
//
// 注意：这里刻意不用 `rmSync(dir, { recursive: true, force: true })`。
// 在中文路径 / ReFS 卷上实测该调用会静默 no-op（不抛错也不删除），
// 导致上一次构建的旧 chunk 被打进 release 二进制。改为逐项 unlink + rmdir，
// 并在删除后断言目录已消失，失败即让构建中断。
const projectRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const distDir = resolve(projectRoot, 'dist')

function measure(dir) {
  let files = 0
  let bytes = 0
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const target = join(dir, entry.name)
    if (entry.isDirectory()) {
      const sub = measure(target)
      files += sub.files
      bytes += sub.bytes
    } else {
      files += 1
      bytes += statSync(target).size
    }
  }
  return { files, bytes }
}

function removeDir(dir) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const target = join(dir, entry.name)
    if (entry.isDirectory()) {
      removeDir(target)
      continue
    }
    try {
      unlinkSync(target)
    } catch (error) {
      // Windows 上只读文件会 EPERM，去掉只读位后重试一次。
      if (error && (error.code === 'EPERM' || error.code === 'EACCES')) {
        chmodSync(target, 0o666)
        unlinkSync(target)
      } else {
        throw error
      }
    }
  }
  rmdirSync(dir)
}

if (!existsSync(distDir)) {
  console.log('[clean-dist] dist 不存在，无需清理')
} else {
  const before = measure(distDir)
  removeDir(distDir)
  if (existsSync(distDir)) {
    throw new Error(`[clean-dist] 清理失败：${distDir} 仍然存在`)
  }
  console.log(
    `[clean-dist] 已清理 dist：${before.files} 个文件 / ${(before.bytes / 1048576).toFixed(2)} MB`
  )
}
