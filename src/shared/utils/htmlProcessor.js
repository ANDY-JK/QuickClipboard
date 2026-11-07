import DOMPurify from 'dompurify'

// style 不再整块禁用（Word/网页粘贴大量依赖行内样式），改为属性白名单：
// 只保留排版与颜色相关的安全属性，其余（position/inset/z-index/background-image/含 url() 的值）一律剥离。
const ALLOWED_STYLE_PROPERTIES = new Set([
  'color',
  'background-color',
  'font-weight',
  'font-style',
  'text-decoration',
  'text-align',
  'font-size',
  'line-height',
  'white-space'
])

// data:image 允许内嵌，但限制体积：超过 512KB 的图片整块换成占位文本。
const MAX_DATA_IMAGE_BYTES = 512 * 1024
const OVERSIZED_IMAGE_PLACEHOLDER = '[图片过大，已省略]'

const PURIFY_CONFIG = {
  ALLOWED_TAGS: [
    'p', 'br', 'strong', 'em', 'u', 's', 'h1', 'h2', 'h3', 'h4', 'h5', 'h6',
    'ul', 'ol', 'li', 'blockquote', 'pre', 'code', 'span', 'div',
    'a', 'img', 'table', 'thead', 'tbody', 'tr', 'th', 'td',
    'b', 'i', 'sub', 'sup', 'strike', 'del', 'ins'
  ],
  // 移除 class，避免粘贴内容借用 UnoCSS 类名伪装成界面控件。
  ALLOWED_ATTR: [
    'style', 'data-image-id', 'src', 'alt', 'title',
    'href', 'target', 'colspan', 'rowspan', 'align', 'valign'
  ],
  // 仅允许 data:image/*，移除 data:text/html 等可执行/可嵌入类型。
  ALLOWED_URI_REGEXP: /^(?:(?:(?:https?|ftps?|mailto|tel|callto|sms|cid|xmpp):)|data:image\/[a-z0-9.+-]+;base64,|[^a-z]|[a-z+.\-]+(?:[^a-z+.\-:]|$))/i,
  KEEP_CONTENT: true,
  RETURN_DOM: false,
  RETURN_DOM_FRAGMENT: false,
  RETURN_TRUSTED_TYPE: false,
  FORBID_TAGS: ['script', 'iframe', 'embed', 'object', 'form', 'input', 'button'],
  FORBID_ATTR: ['onerror', 'onload', 'onclick', 'onmouseover', 'onfocus', 'onblur']
}

function filterStyleAttribute(styleValue) {
  const kept = []
  for (const declaration of String(styleValue).split(';')) {
    const separatorIndex = declaration.indexOf(':')
    if (separatorIndex < 0) continue

    const property = declaration.slice(0, separatorIndex).trim().toLowerCase()
    const value = declaration.slice(separatorIndex + 1).trim()
    if (!value) continue
    if (!ALLOWED_STYLE_PROPERTIES.has(property)) continue
    // url()/expression() 等可加载外部资源或执行脚本的值一律丢弃。
    if (/url\s*\(|expression\s*\(|@import|javascript:/i.test(value)) continue
    kept.push(`${property}: ${value}`)
  }
  return kept.join('; ')
}

function isOversizedDataImage(src) {
  if (!src || !/^data:image\/[a-z0-9.+-]+;base64,/i.test(src)) return false
  // base64 字符数约等于字节数的 4/3，这里保守按 3/4 折算。
  return src.length * 0.75 > MAX_DATA_IMAGE_BYTES
}

function postProcessSanitizedHtml(html) {
  if (typeof document === 'undefined') return html

  const template = document.createElement('template')
  template.innerHTML = html

  template.content.querySelectorAll('[style]').forEach((element) => {
    const filtered = filterStyleAttribute(element.getAttribute('style'))
    if (filtered) {
      element.setAttribute('style', filtered)
    } else {
      element.removeAttribute('style')
    }
  })

  template.content.querySelectorAll('img').forEach((image) => {
    if (!isOversizedDataImage(image.getAttribute('src'))) return
    image.replaceWith(document.createTextNode(OVERSIZED_IMAGE_PLACEHOLDER))
  })

  return template.innerHTML
}

// 使用 DOMPurify 清理 HTML 内容
export function sanitizeHTML(htmlContent) {
  const sanitized = DOMPurify.sanitize(htmlContent, PURIFY_CONFIG)
  return postProcessSanitizedHtml(sanitized)
}
