function rawErrorMessage(error) {
  if (error == null) return '';
  if (typeof error === 'string') return error;
  if (typeof error.message === 'string') return error.message;
  try {
    return String(error);
  } catch {
    return '';
  }
}

function hasAny(text, values) {
  return values.some((value) => text.includes(value));
}

// 仅保留仍在使用中的文件错误匹配规则；WebDAV / 局域网 / 转移架 / 收件盒
// 相关文案已随功能移除，全部删除。
const EXACT_RULES = [
  { patterns: ['下载文件校验失败', '局域网文件内容校验失败', '局域网文件大小校验失败'], key: 'errors.file.checksumFailed' },
  { patterns: ['云端文件过大', '云端加密文件大小溢出'], key: 'errors.file.tooLarge' },
  { patterns: ['待上传文件大小发生变化'], key: 'errors.file.changedDuringTransfer' },
  { patterns: ['只能上传普通文件', '只能传输普通文件', '只能操作收件盒内的普通文件'], key: 'errors.file.onlyRegularFile' },
  { patterns: ['文件不存在或无法访问', '文件不存在'], key: 'errors.file.notFound' },
  { patterns: ['文件名无效', '文件名包含非法字符', '文件名编码无效'], key: 'errors.file.invalidName' },
];

const CONTAINS_RULES = [
  { patterns: ['打开待上传文件失败', '读取待上传文件信息失败'], key: 'errors.file.readFailed' },
  { patterns: ['计算文件校验值失败'], key: 'errors.file.hashFailed' },
  { patterns: ['创建下载目录失败', '创建本地下载文件失败', '保存本地下载文件失败', '替换本地下载文件失败'], key: 'errors.file.saveFailed' },
  { patterns: ['打开文件失败'], key: 'errors.file.openFailed' },
  { patterns: ['打开文件位置失败'], key: 'errors.file.revealFailed' },
  { patterns: ['删除本地文件失败'], key: 'errors.file.deleteFailed' },
];

function matchKnownMessage(raw, t, fallbackKey) {
  const lower = raw.toLowerCase();

  for (const rule of EXACT_RULES) {
    if (hasAny(raw, rule.patterns)) return t(rule.key);
  }

  for (const rule of CONTAINS_RULES) {
    if (hasAny(raw, rule.patterns)) return t(rule.key);
  }

  if (hasAny(lower, ['timed out', 'timeout', 'operation timed out', '请求超时'])) {
    return t(fallbackKey);
  }
  if (hasAny(lower, ['connection refused', 'dns error', 'failed to lookup', 'error sending request', 'tcp connect error', 'network'])) {
    return t(fallbackKey);
  }

  return t(fallbackKey);
}

export function formatUserMessage(error, t, fallbackKey = 'errors.operationFailed') {
  const raw = rawErrorMessage(error).trim();
  if (!raw) return t(fallbackKey);
  return matchKnownMessage(raw, t, fallbackKey);
}

export function formatUserMessages(errors, t, fallbackKey = 'errors.operationFailed') {
  if (!Array.isArray(errors)) return [];
  return errors
    .map((error) => formatUserMessage(error, t, fallbackKey))
    .filter(Boolean);
}
