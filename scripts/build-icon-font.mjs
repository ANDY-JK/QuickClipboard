/**
 * 生成 Tabler Icons 真子集（woff2）与对应 CSS。
 *
 * 流程：
 *   1. 从 node_modules 的官方 CSS 读取「图标名 -> 码点」映射；
 *   2. 扫描 src 下所有 ti-* 类名，并合并现有 CSS 中的类名（保留运行时动态图标）；
 *   3. 用 subset-font（harfbuzz wasm）裁出只含这些码点的 woff2；
 *   4. 用 fontkit 校验每个类名的码点都真的落在子集 cmap 里；
 *   5. 写出字体与 CSS @font-face。
 *
 * 新增图标后执行：npm run build:icons（npm run build 的 prebuild 也会自动执行）。
 */
import { existsSync, mkdirSync, readdirSync, readFileSync, statSync, writeFileSync } from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import subsetFont from 'subset-font'
import * as fontkit from 'fontkit'

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const SOURCE_FONT = path.join(ROOT, 'node_modules/@tabler/icons-webfont/dist/fonts/tabler-icons.woff2')
const OFFICIAL_CSS = path.join(ROOT, 'node_modules/@tabler/icons-webfont/dist/tabler-icons.css')
const OUT_FONT_DIR = path.join(ROOT, 'src/shared/styles/fonts')
const OUT_FONT = path.join(OUT_FONT_DIR, 'tabler-icons-subset.woff2')
const OUT_CSS = path.join(ROOT, 'src/shared/styles/tabler-icons-subset.css')
const SCAN_ROOT = path.join(ROOT, 'src')
const SCAN_EXTS = new Set(['.js', '.jsx', '.ts', '.tsx', '.css', '.html', '.json'])
const ICON_RULE_RE = /\.ti-([a-z0-9-]+):before\s*\{\s*content:\s*"\\([0-9a-fA-F]+)"/g

function walk(dir, files = []) {
  for (const name of readdirSync(dir)) {
    const full = path.join(dir, name)
    if (statSync(full).isDirectory()) {
      walk(full, files)
    } else {
      files.push(full)
    }
  }
  return files
}

function parseIconMap(css) {
  const map = new Map()
  for (const match of css.matchAll(ICON_RULE_RE)) {
    map.set(match[1], parseInt(match[2], 16))
  }
  return map
}

function kilobytes(bytes) {
  return `${(bytes / 1024).toFixed(1)} KB`
}

if (!existsSync(SOURCE_FONT) || !existsSync(OFFICIAL_CSS)) {
  console.error('[icon-font] 缺少 @tabler/icons-webfont，请先 npm install')
  process.exit(1)
}

const officialMap = parseIconMap(readFileSync(OFFICIAL_CSS, 'utf8'))
const existingCss = readFileSync(OUT_CSS, 'utf8')
const existingMap = parseIconMap(existingCss)

if (officialMap.size === 0) {
  console.error('[icon-font] 无法从官方 CSS 解析图标映射')
  process.exit(1)
}

const scannedNames = new Set()
const unknownNames = new Set()
for (const file of walk(SCAN_ROOT)) {
  if (!SCAN_EXTS.has(path.extname(file))) continue
  const content = readFileSync(file, 'utf8')
  for (const match of content.matchAll(/\bti-([a-z0-9-]+)\b/g)) {
    scannedNames.add(match[1])
  }
}

// 现有 CSS 里的类名视为允许清单：其中有用户配置里动态引用、源码中无字面量的图标。
const finalNames = new Set(existingMap.keys())
let appended = 0
for (const name of scannedNames) {
    if (!officialMap.has(name)) {
      unknownNames.add(name)
      continue
    }
    if (!finalNames.has(name)) {
      finalNames.add(name)
      appended += 1
    }
}

const unknownExisting = [...existingMap.keys()].filter((name) => !officialMap.has(name))
if (unknownExisting.length > 0) {
  console.error(`[icon-font] 现有 CSS 中的图标在官方字体里找不到（@tabler 版本可能已变化）：${unknownExisting.join(', ')}`)
  process.exit(1)
}

const entries = [...finalNames]
  .map((name) => [name, officialMap.get(name)])
  .sort(([a], [b]) => a.localeCompare(b))

const text = entries.map(([, codePoint]) => String.fromCodePoint(codePoint)).join('')
const sourceBuffer = readFileSync(SOURCE_FONT)

// 先用 sfnt 产物做覆盖校验（fontkit 不支持直接解析 woff2 的 cmap）。
const verifyBuffer = await subsetFont(sourceBuffer, text, { targetFormat: 'sfnt' })
const verifyFont = fontkit.create(verifyBuffer)
const missing = entries.filter(([name, codePoint]) => !verifyFont.hasGlyphForCodePoint(codePoint))
if (missing.length > 0) {
  console.error(`[icon-font] 子集缺少字形：${missing.map(([name]) => name).join(', ')}`)
  process.exit(1)
}

const subsetBuffer = await subsetFont(sourceBuffer, text, { targetFormat: 'woff2' })
mkdirSync(OUT_FONT_DIR, { recursive: true })
writeFileSync(OUT_FONT, subsetBuffer)

const rules = entries
  .map(([name, codePoint]) => `.ti-${name}:before{content:"\\${codePoint.toString(16)}"}`)
  .join('')

writeFileSync(
  OUT_CSS,
  `/*! Tabler Icons 子集（由 scripts/build-icon-font.mjs 生成，勿手改）\n` +
    ` *  重新生成：npm run build:icons（新增 ti-* 图标后执行，build 的 prebuild 也会自动执行）。\n` +
    ` *  仅保留 woff2，避免额外打包 ttf/woff。\n` +
    ` */\n` +
    `@font-face{font-family:"tabler-icons";font-style:normal;font-weight:400;font-display:block;src:url("./fonts/tabler-icons-subset.woff2") format("woff2")}\n` +
    `.ti{font-family:"tabler-icons" !important;speak:none;font-style:normal;font-weight:normal;font-variant:normal;text-transform:none;line-height:1;-webkit-font-smoothing:antialiased;-moz-osx-font-smoothing:grayscale}\n` +
    `${rules}\n`
)

if (unknownNames.size > 0) {
  console.warn(`[icon-font] 源码中以下 ti-* 不在官方图标集中（已忽略）：${[...unknownNames].join(', ')}`)
}

console.log(
  `[icon-font] ${entries.length} 个图标（新增 ${appended}）｜` +
    `woff2 ${kilobytes(sourceBuffer.length)} → ${kilobytes(subsetBuffer.length)}｜覆盖校验 ${entries.length}/${entries.length} 通过`
)
