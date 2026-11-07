#!/usr/bin/env node
/**
 * 构建期类名守卫：确保 src/** 中写出的每个 class 都能被 UnoCSS 真正生成。
 *
 * 背景：UnoCSS 对无法解析的类名不会报错，只会静默不生成样式，
 * 导致「类名写了但样式没生效、元素错位」的问题很难被发现。
 *
 * 判定口径（与真实构建保持同源）：
 *   1. 使用项目根目录真实的 uno.config.js 构造 generator；
 *   2. 用 generator 的 matched 结果判定类名是否有对应规则；
 *   3. 项目自有 CSS（src/**\/*.css 与 html 内联 <style>）中定义过的类名视为合法；
 *   4. 少量非 UnoCSS 语义类按脚本内注明的规则豁免。
 *
 * 用法：node scripts/check-unocss-classes.mjs
 * 退出码：0 = 通过；1 = 存在未生成的类名或脚本执行失败。
 */

import { readdirSync, readFileSync, statSync } from 'node:fs'
import { join, relative, resolve } from 'node:path'
import { pathToFileURL } from 'node:url'
import { performance } from 'node:perf_hooks'
import { createGenerator } from '@unocss/core'

const ROOT = process.cwd()
const SRC_DIR = resolve(ROOT, 'src')
const CODE_EXTENSIONS = new Set(['.js', '.jsx', '.ts', '.tsx'])
const SKIP_DIRS = new Set(['node_modules', 'dist', 'coverage', '__snapshots__', 'fixtures', 'samples', 'test-samples'])

/**
 * 豁免规则。每一条都必须写明「为什么它不是 UnoCSS 该管的类」，
 * 不允许堆一长串具体类名来掩盖真实问题。
 *
 * 注意：曾经有一条「以 list/container/surface 等语义后缀结尾就豁免」的启发式规则，
 * 它会连带放过 bg-qc-surface 这类真实样式类名，已改为「必须被选择器语境引用过」
 * （见 scanFile 里的 selectorHooks 收集）。
 */
const EXEMPT_RULES = [
  {
    name: 'Tabler Icons 字体类',
    test: (token) => token === 'ti' || /^ti-[a-z0-9-]+$/.test(token),
    reason: '图标字体类由 @tabler/icons-webfont 的字体 CSS 提供，不由 UnoCSS 生成',
  },
  {
    name: 'UnoCSS 分组标记',
    test: (token) => token === 'group' || token === 'peer',
    reason: 'group/peer 是 UnoCSS 变体标记，本身不需要生成样式规则',
  },
]

// UnoCSS 单字工具类白名单：表达式里的字符串只有在「像 class」时才参与检查，
// 避免把条件值（如 'text'、'replace'、'merge'）误报成类名。
const SINGLE_WORD_UTILITIES = new Set([
  'flex', 'grid', 'block', 'inline', 'hidden', 'table', 'contents',
  'relative', 'absolute', 'fixed', 'sticky', 'static',
  'underline', 'italic', 'truncate', 'uppercase', 'lowercase', 'capitalize',
  'border', 'rounded', 'shadow', 'ring', 'outline',
  'grow', 'shrink', 'transform', 'transition', 'filter', 'isolate',
  'container', 'visible', 'invisible', 'collapse',
])

function walkFiles(dir, files = []) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    if (entry.isDirectory()) {
      if (!SKIP_DIRS.has(entry.name)) walkFiles(join(dir, entry.name), files)
    } else if (entry.isFile()) {
      files.push(join(dir, entry.name))
    }
  }
  return files
}

function stripCssComments(text) {
  return text.replace(/\/\*[\s\S]*?\*\//g, '')
}

function collectClassSelectors(text, target) {
  const cleaned = stripCssComments(text)
  // 复合选择器（如 .theme-option-button.is-active）里的后置类名同样有效，
  // 因此不能要求「.」前必须是空白；数字开头（0.5em）不会被 [A-Za-z_] 命中。
  for (const match of cleaned.matchAll(/\.[A-Za-z_][A-Za-z0-9_-]*/g)) {
    target.add(match[0].slice(1))
  }
}

/** 项目自有 CSS 里出现过的类名（含 html 内联 <style> 与 JSX 内的 <style> 块），视为合法。 */
function collectCustomCssClasses(files) {
  const classes = new Set()
  for (const file of files) {
    const ext = file.slice(file.lastIndexOf('.'))
    if (ext === '.css') {
      collectClassSelectors(readFileSync(file, 'utf8'), classes)
    } else if (ext === '.html' || CODE_EXTENSIONS.has(ext)) {
      const content = readFileSync(file, 'utf8')
      for (const match of content.matchAll(/<style\b[^>]*>([\s\S]*?)<\/style>/gi)) {
        collectClassSelectors(match[1], classes)
      }
    }
  }
  return classes
}

function makeLineLocator(content) {
  const starts = [0]
  for (let i = 0; i < content.length; i++) {
    if (content.charCodeAt(i) === 10) starts.push(i + 1)
  }
  return (index) => {
    let lo = 0
    let hi = starts.length - 1
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1
      if (starts[mid] <= index) lo = mid
      else hi = mid - 1
    }
    return lo + 1
  }
}

/** 读取普通引号字符串，返回 { value, end }。 */
function readQuoted(source, start) {
  const quote = source[start]
  let i = start + 1
  let value = ''
  while (i < source.length) {
    const ch = source[i]
    if (ch === '\\') {
      value += source[i + 1] ?? ''
      i += 2
      continue
    }
    if (ch === quote) return { value, end: i + 1 }
    value += ch
    i++
  }
  return { value, end: source.length }
}

/** 跳过 `${ ... }` 插值块，返回闭合 `}` 之后的位置。 */
function skipInterpolation(source, start) {
  let i = start // 指向 `{`
  let depth = 0
  while (i < source.length) {
    const ch = source[i]
    if (ch === '"' || ch === "'") {
      i = readQuoted(source, i).end
      continue
    }
    if (ch === '`') {
      i = readTemplate(source, i).end
      continue
    }
    if (ch === '/' && source[i + 1] === '/') {
      const nl = source.indexOf('\n', i)
      i = nl === -1 ? source.length : nl + 1
      continue
    }
    if (ch === '/' && source[i + 1] === '*') {
      const close = source.indexOf('*/', i + 2)
      i = close === -1 ? source.length : close + 2
      continue
    }
    if (ch === '{') depth++
    else if (ch === '}') {
      depth--
      if (depth === 0) return i + 1
    }
    i++
  }
  return source.length
}

/** 读取模板字符串，返回 { value, end, interpolation }。含 ${} 时整体跳过。 */
function readTemplate(source, start) {
  let i = start + 1
  let value = ''
  let interpolation = false
  while (i < source.length) {
    const ch = source[i]
    if (ch === '\\') {
      value += source[i + 1] ?? ''
      i += 2
      continue
    }
    if (ch === '`') return { value, end: i + 1, interpolation }
    if (ch === '$' && source[i + 1] === '{') {
      interpolation = true
      i = skipInterpolation(source, i + 1)
      continue
    }
    value += ch
    i++
  }
  return { value, end: source.length, interpolation }
}

/**
 * 读取模板字符串的各个片段：字面量 text 与插值 expr，并保留各自的源码偏移。
 * 这样写是为了让模板里的字面量类名也能逐个校验，而不是整串跳过。
 */
function readTemplateParts(source, start) {
  let i = start + 1
  const parts = []
  let text = ''
  let textStart = i
  const flushText = () => {
    if (text) parts.push({ type: 'text', value: text, start: textStart })
    text = ''
  }
  while (i < source.length) {
    const ch = source[i]
    if (ch === '\\') {
      text += source[i + 1] ?? ''
      i += 2
      continue
    }
    if (ch === '`') {
      flushText()
      return { parts, end: i + 1 }
    }
    if (ch === '$' && source[i + 1] === '{') {
      flushText()
      const bodyStart = i + 2
      const after = skipInterpolation(source, i + 1)
      parts.push({ type: 'expr', body: source.slice(bodyStart, Math.max(bodyStart, after - 1)), start: bodyStart })
      i = after
      continue
    }
    if (!text) textStart = i
    text += ch
    i++
  }
  flushText()
  return { parts, end: source.length }
}

/** 读取 `{ ... }` 表达式，返回 { body, bodyStart, end }。 */
function readBraceExpression(source, start) {
  let i = start // 指向 `{`
  let depth = 0
  const bodyStart = start + 1
  while (i < source.length) {
    const ch = source[i]
    if (ch === '"' || ch === "'") {
      i = readQuoted(source, i).end
      continue
    }
    if (ch === '`') {
      i = readTemplate(source, i).end
      continue
    }
    if (ch === '/' && source[i + 1] === '/') {
      const nl = source.indexOf('\n', i)
      i = nl === -1 ? source.length : nl + 1
      continue
    }
    if (ch === '/' && source[i + 1] === '*') {
      const close = source.indexOf('*/', i + 2)
      i = close === -1 ? source.length : close + 2
      continue
    }
    if (ch === '{') depth++
    else if (ch === '}') {
      depth--
      if (depth === 0) return { body: source.slice(bodyStart, i), bodyStart, end: i + 1 }
    }
    i++
  }
  return { body: source.slice(bodyStart), bodyStart, end: source.length }
}

/** 读取 `( ... )` 参数，返回 { body, bodyStart, end }。 */
function readParenBody(source, start) {
  let i = start // 指向 `(`
  let depth = 0
  const bodyStart = start + 1
  while (i < source.length) {
    const ch = source[i]
    if (ch === '"' || ch === "'") {
      i = readQuoted(source, i).end
      continue
    }
    if (ch === '`') {
      i = readTemplate(source, i).end
      continue
    }
    if (ch === '(') depth++
    else if (ch === ')') {
      depth--
      if (depth === 0) return { body: source.slice(bodyStart, i), bodyStart, end: i + 1 }
    }
    i++
  }
  return { body: source.slice(bodyStart), bodyStart, end: source.length }
}

function isLikelyClassToken(token) {
  if (SINGLE_WORD_UTILITIES.has(token)) return true
  return /[!:/[\]%.-]/.test(token)
}

function isComparisonOperand(source, stringStart) {
  const before = source.slice(Math.max(0, stringStart - 10), stringStart)
  return /(?:===|!==|==|!=)\s*$/.test(before)
}

function isI18nKeyCall(source, stringStart) {
  const before = source.slice(Math.max(0, stringStart - 24), stringStart)
  return /(?:^|[^\w$])(?:tc|t)\s*\(\s*$/.test(before)
}

function createCollector(file, lineLocator, occurrences, dynamic, runtime, templates) {

  /** A 类：插值体里都有字符串字面量，可静态判定；B 类：插值来自变量/函数/配置。 */
  function classifyParts(parts) {
    return parts.some((part) => part.type === 'expr' && !/['"`]/.test(part.body)) ? 'B' : 'A'
  }

  function noteTemplate(parts, line, raw) {
    if (parts.some((part) => part.type === 'expr')) {
      templates.push({ file, line, raw, kind: classifyParts(parts) })
    }
  }
  const seen = new Set()

  function addToken(token, index, expression) {
    if (!token) return
    if (token.includes('${')) return
    if (expression && !isLikelyClassToken(token)) return
    const line = lineLocator(index)
    const key = `${file}:${line}:${token}`
    if (seen.has(key)) return
    seen.add(key)
    occurrences.push({ file, line, token })
  }

  function addClassString(value, absoluteStart, expression, cutStart = false, cutEnd = false) {
    const re = /\S+/g
    const tokens = []
    let match
    while ((match = re.exec(value)) !== null) {
      tokens.push({ token: match[0], index: match.index })
    }
    for (let k = 0; k < tokens.length; k++) {
      const isFirst = k === 0
      const isLast = k === tokens.length - 1
      // 与插值紧邻的边界 token 可能是「类名片段」（如 ti-${name} 里的 ti-），
      // 无法静态判定，计入动态片段而不是当成完整类名。
      if ((cutStart && isFirst) || (cutEnd && isLast)) {
        dynamic.push({
          file,
          line: lineLocator(absoluteStart + tokens[k].index),
          token: tokens[k].token,
          raw: value,
        })
        continue
      }
      addToken(tokens[k].token, absoluteStart + tokens[k].index, expression)
    }
  }

  /** 逐片段处理模板字符串：字面量部分照常校验，插值部分递归解析其内部的字符串。 */
  function addTemplateParts(parts, expression, offset = 0) {
    for (let idx = 0; idx < parts.length; idx++) {
      const part = parts[idx]
      const absoluteStart = part.start + offset
      if (part.type === 'expr') {
        // 插值体里没有任何字符串字面量，说明类名由变量/函数/配置在运行时决定，无法静态校验。
        if (!/['"`]/.test(part.body)) {
          runtime.push({ file, line: lineLocator(absoluteStart), body: part.body })
        }
        addExpression(part.body, absoluteStart)
        continue
      }
      const prevExpr = idx > 0 && parts[idx - 1].type === 'expr'
      const nextExpr = idx + 1 < parts.length && parts[idx + 1].type === 'expr'
      // 只有「紧贴插值且没有空白」的边界才有可能是片段；有空白说明前一个 token 已完整结束。
      addClassString(part.value, absoluteStart, expression, prevExpr && !/^\s/.test(part.value), nextExpr && !/\s$/.test(part.value))
    }
  }

  function addExpression(body, bodyStart) {
    let i = 0
    while (i < body.length) {
      const ch = body[i]
      if (ch === '/' && body[i + 1] === '/') {
        const nl = body.indexOf('\n', i)
        i = nl === -1 ? body.length : nl + 1
        continue
      }
      if (ch === '/' && body[i + 1] === '*') {
        const close = body.indexOf('*/', i + 2)
        i = close === -1 ? body.length : close + 2
        continue
      }
      if (ch === '"' || ch === "'") {
        const string = readQuoted(body, i)
        if (!isComparisonOperand(body, i) && !isI18nKeyCall(body, i)) {
          addClassString(string.value, bodyStart + i, true)
        }
        i = string.end
        continue
      }
      if (ch === '`') {
        const template = readTemplateParts(body, i)
        noteTemplate(template.parts, lineLocator(bodyStart + i), body.slice(i, template.end))
        addTemplateParts(template.parts, true, bodyStart)
        i = template.end
        continue
      }
      i++
    }
  }

  return { addClassString, addExpression, addTemplate: addTemplateParts, noteTemplate }
}

function scanFile(file, occurrences, dynamic, selectorHooks, runtime, templates) {
  const content = readFileSync(file, 'utf8')
  const lineLocator = makeLineLocator(content)
  const collector = createCollector(file, lineLocator, occurrences, dynamic, runtime, templates)

  // DOM 钩子：只有真正出现在选择器语境里的类名才豁免。
  // 覆盖 querySelector/closest/matches 的入参，以及任意「以 . 开头的选择器字符串」
  // （如 containerSelector: '.preview-theme-anchor'、clearBackgroundImage('.preview-theme-anchor')）。
  const selectorPatterns = [
    /(?:querySelector(?:All)?|closest|matches)\s*\(\s*(['"`])([^'"`]+)\1/g,
    /(['"`])([.#][A-Za-z0-9_-][^'"`]*)\1/g,
  ]
  for (const pattern of selectorPatterns) {
    for (const hook of content.matchAll(pattern)) {
      collectClassSelectors(hook[2], selectorHooks)
    }
  }

  // className="..." / className='...' / className={`...`} / className={...}
  const attrPattern = /(?<![\w$])(?:className|class)\s*=\s*/g
  let match
  while ((match = attrPattern.exec(content)) !== null) {
    let i = attrPattern.lastIndex
    const ch = content[i]
    if (ch === '"' || ch === "'") {
      const string = readQuoted(content, i)
      collector.addClassString(string.value, i + 1, false)
      attrPattern.lastIndex = string.end
    } else if (ch === '`') {
      const template = readTemplateParts(content, i)
      collector.noteTemplate(template.parts, lineLocator(i), content.slice(i, template.end))
      collector.addTemplate(template.parts, false)
      attrPattern.lastIndex = template.end
    } else if (ch === '{') {
      const expression = readBraceExpression(content, i)
      collector.addExpression(expression.body, expression.bodyStart)
      attrPattern.lastIndex = expression.end
    }
  }

  // clsx(...) / cn(...)：项目当前未使用，但保留支持，避免以后新增后漏检。
  const fnPattern = /(?<![\w$])(?:clsx|cn)\s*\(/g
  while ((match = fnPattern.exec(content)) !== null) {
    const paren = readParenBody(content, fnPattern.lastIndex - 1)
    collector.addExpression(paren.body, paren.bodyStart)
    fnPattern.lastIndex = paren.end
  }
}

function isExempt(token, selectorHooks) {
  if (selectorHooks.has(token)) return { name: 'DOM 选择器钩子' }
  for (const rule of EXEMPT_RULES) {
    if (rule.test(token)) return rule
  }
  return null
}

function printFailure(misses, stats) {
  console.error('')
  console.error(`✗ [unocss-class-check] 发现 ${misses.length} 处类名没有 UnoCSS 规则，也没有在项目 CSS 中定义，构建已终止。`)
  console.error(
    `  扫描 ${stats.fileCount} 个文件 / ${stats.occurrenceCount} 处类名出现（去重 ${stats.uniqueCount} 个）` +
      ` / 动态类名跳过 ${stats.dynamicCount} 处 / 运行时插值 ${stats.runtimeCount} 处 / 项目 CSS 自定义类 ${stats.cssClassCount} 个` +
      ` / 语义类豁免 ${stats.exemptCount} 个`,
  )
  console.error('')

  const byFile = new Map()
  for (const miss of misses) {
    if (!byFile.has(miss.file)) byFile.set(miss.file, [])
    byFile.get(miss.file).push(miss)
  }
  for (const [file, items] of [...byFile.entries()].sort((a, b) => a[0].localeCompare(b[0]))) {
    console.error(relative(ROOT, file).replace(/\\/g, '/'))
    for (const item of items.sort((a, b) => a.line - b.line || a.token.localeCompare(b.token))) {
      console.error(`  L${item.line}  ${item.token}`)
    }
    console.error('')
  }

  console.error('提示：这些类名不会报错、只会静默失效；请检查拼写、主题 token 结构或改用任意值写法（如 text-[var(--qc-fg-muted)]）。')
}

function printDynamicList(dynamic, runtime = [], templates = []) {
  const cache = new Map()
  const byFile = new Map()
  const push = (file, item) => {
    if (!byFile.has(file)) byFile.set(file, [])
    byFile.get(file).push(item)
  }
  for (const item of dynamic) push(item.file, { ...item, kind: '片段' })
  for (const item of runtime) push(item.file, { ...item, kind: '运行时' })
  for (const item of templates) push(item.file, { ...item, token: item.kind, kind: '模板' })
  const counts = templates.reduce((acc, item) => {
    acc[item.kind] = (acc[item.kind] ?? 0) + 1
    return acc
  }, {})
  console.log(
    `模板含插值 ${templates.length} 处（A=${counts.A ?? 0} / B=${counts.B ?? 0}）` +
      ` / 动态类名片段 ${dynamic.length} 处 / 运行时插值 ${runtime.length} 处`,
  )
  console.log('')
  for (const [file, items] of [...byFile.entries()].sort((a, b) => a[0].localeCompare(b[0]))) {
    console.log(relative(ROOT, file).replace(/\\/g, '/'))
    if (!cache.has(file)) cache.set(file, readFileSync(file, 'utf8').split(/\r?\n/))
    const lines = cache.get(file)
    for (const item of items.sort((a, b) => a.line - b.line)) {
      const raw = (item.raw ?? (lines[item.line - 1] ?? '').trim()).replace(/\s+/g, ' ')
      console.log(`  L${item.line}  [${item.kind}] ${item.token}  <-  ${raw}`)
    }
    console.log('')
  }
}

function printPass(stats) {
  const seconds = (stats.durationMs / 1000).toFixed(2)
  console.log(
    `✓ [unocss-class-check] 通过：扫描 ${stats.fileCount} 个文件 / ${stats.occurrenceCount} 处类名出现（去重 ${stats.uniqueCount} 个）` +
      ` / 动态类名跳过 ${stats.dynamicCount} 处 / 运行时插值 ${stats.runtimeCount} 处 / 项目 CSS 自定义类 ${stats.cssClassCount} 个` +
      ` / 语义类豁免 ${stats.exemptCount} 个 / 耗时 ${seconds}s`,
  )
}

async function main() {
  const startedAt = performance.now()
  if (!statSync(SRC_DIR, { throwIfNoEntry: false })?.isDirectory()) {
    console.error(`✗ [unocss-class-check] 找不到源码目录：${SRC_DIR}`)
    process.exit(1)
  }

  const allFiles = walkFiles(SRC_DIR)
  const codeFiles = allFiles.filter((file) => CODE_EXTENSIONS.has(file.slice(file.lastIndexOf('.'))))
  const customCssClasses = collectCustomCssClasses(allFiles)
  const selectorHooks = new Set()

  const occurrences = []
  const dynamic = []
  const runtime = []
  const templates = []
  for (const file of codeFiles) scanFile(file, occurrences, dynamic, selectorHooks, runtime, templates)

  // 诊断模式：只列出无法静态判定的动态类名片段与运行时插值，不做生成与判定。
  // 用于人工梳理风险清单，不参与通过/失败判定。
  if (process.argv.includes('--list-dynamic')) {
    printDynamicList(dynamic, runtime, templates)
    return
  }

  const uniqueTokens = [...new Set(occurrences.map((item) => item.token))]

  let config
  try {
    const configModule = await import(pathToFileURL(resolve(ROOT, 'uno.config.js')).href)
    config = configModule.default
  } catch (error) {
    console.error(`✗ [unocss-class-check] 加载 uno.config.js 失败：${error.message}`)
    process.exit(1)
  }

  const generator = await createGenerator(config)

  // 一次性生成全部 token；matched 只包含真正命中规则的输入 token。
  const matched = new Set()
  const BATCH_SIZE = 400
  for (let i = 0; i < uniqueTokens.length; i += BATCH_SIZE) {
    const batch = uniqueTokens.slice(i, i + BATCH_SIZE).join(' ')
    const result = await generator.generate(batch, { preflights: false })
    for (const token of result.matched) matched.add(token)
  }

  const misses = []
  let exemptCount = 0
  for (const item of occurrences) {
    if (matched.has(item.token)) continue
    if (customCssClasses.has(item.token)) continue
    if (isExempt(item.token, selectorHooks)) continue
    misses.push(item)
  }
  for (const token of uniqueTokens) {
    if (matched.has(token) || customCssClasses.has(token)) continue
    if (isExempt(token, selectorHooks)) exemptCount++
  }

  const stats = {
    fileCount: codeFiles.length,
    occurrenceCount: occurrences.length,
    uniqueCount: uniqueTokens.length,
    dynamicCount: dynamic.length,
    runtimeCount: runtime.length,
    cssClassCount: customCssClasses.size,
    exemptCount,
    durationMs: performance.now() - startedAt,
  }

  if (misses.length > 0) {
    printFailure(misses, stats)
    process.exit(1)
  }
  printPass(stats)
}

main().catch((error) => {
  console.error(`✗ [unocss-class-check] 执行失败：${error?.stack || error}`)
  process.exit(1)
})
