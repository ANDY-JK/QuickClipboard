// 中英两份 JSON 合计约 841KB，静态 import 会让它们进入首屏依赖图。
// 这里改为动态 import，并且只加载当前语言的一份；切换语言时再按需加载另一份。

export const GROUP_ID_MAP = {
  0: 'smileys-emotion',
  1: 'people-body',
  3: 'animals-nature',
  4: 'food-drink',
  5: 'travel-places',
  6: 'activities',
  7: 'objects',
  8: 'symbols'
};

export const SYMBOL_RANGES = {
  punctuation: [[0x3000, 0x303F], [0xFF01, 0xFF5E]],
  arrows:     [[0x2190, 0x21FF], [0x27F0, 0x27FF], [0x2900, 0x297F]],
  math:       [[0x2200, 0x22FF], [0x27C0, 0x27EF], [0x2980, 0x29FF]],
  currency:   [[0x20A0, 0x20CF]],
  geometric:  [[0x25A0, 0x25FF]],
  box:        [[0x2500, 0x257F]],
  misc:       [[0x2600, 0x26FF], [0x2700, 0x27BF]],
  technical:  [[0x2300, 0x23FF]],
  letterlike: [[0x2100, 0x214F]],
  numbers:    [[0x2460, 0x24FF]],
};

export const PUNCTUATION_MANUAL = [
  '!', '"', '#', '$', '%', '&', "'", '(', ')', '*', '+', ',', '-', '.', '/',
  ':', ';', '<', '=', '>', '?', '@', '[', '\\', ']', '^', '_', '`', '{', '|', '}', '~',
  '\u2018', '\u2019', '\u201C', '\u201D', '…', '—', '–', '·', '•', '©', '®', '™', '°', '±', '×', '÷',
];

function scanRanges(ranges) {
  const arr = [];
  for (const [start, end] of ranges) {
    for (let cp = start; cp <= end; cp++) {
      try {
        const ch = String.fromCodePoint(cp);
        if (!/\s/.test(ch) && ch.trim()) arr.push(ch);
      } catch (e) {}
    }
  }
  return arr;
}

export const symbolCategories = {};
for (const name in SYMBOL_RANGES) {
  symbolCategories[name] = scanRanges(SYMBOL_RANGES[name]);
}
symbolCategories['punctuation'] = [...PUNCTUATION_MANUAL, ...symbolCategories['punctuation']];

export const SYMBOL_CATS = [
  { id: 'punctuation', icon: 'ti-quote', labelKey: 'emoji.cat.punctuation' },
  { id: 'arrows', icon: 'ti-arrow-right', labelKey: 'emoji.cat.arrows' },
  { id: 'math', icon: 'ti-math', labelKey: 'emoji.cat.math' },
  { id: 'currency', icon: 'ti-currency-dollar', labelKey: 'emoji.cat.currency' },
  { id: 'geometric', icon: 'ti-cube', labelKey: 'emoji.cat.geometric' },
  { id: 'box', icon: 'ti-square-dashed', labelKey: 'emoji.cat.box' },
  { id: 'misc', icon: 'ti-star', labelKey: 'emoji.cat.misc' },
  { id: 'technical', icon: 'ti-settings', labelKey: 'emoji.cat.technical' },
  { id: 'letterlike', icon: 'ti-letter-a', labelKey: 'emoji.cat.letterlike' },
  { id: 'numbers', icon: 'ti-number', labelKey: 'emoji.cat.numbers' },
];

export const EMOJI_CATS = [
  { id: 'recent', icon: 'ti-clock', labelKey: 'emoji.recent' },
  { id: 'smileys-emotion', icon: 'ti-mood-smile', labelKey: 'emoji.smileys' },
  { id: 'people-body', icon: 'ti-user', labelKey: 'emoji.people' },
  { id: 'animals-nature', icon: 'ti-paw', labelKey: 'emoji.animals' },
  { id: 'food-drink', icon: 'ti-apple', labelKey: 'emoji.food' },
  { id: 'travel-places', icon: 'ti-plane', labelKey: 'emoji.travel' },
  { id: 'activities', icon: 'ti-ball-football', labelKey: 'emoji.activities' },
  { id: 'objects', icon: 'ti-bulb', labelKey: 'emoji.objects' },
  { id: 'symbols', icon: 'ti-abc', labelKey: 'emoji.symbolsCat' },
];

export const RECENT_KEY = 'emoji_recent_v1';
export const SKIN_TONE_KEY = 'emoji_skin_tone';
export const MAX_RECENT = 32;

export const SKIN_TONES = [
  { id: 'default', label: 'Default' },
  { id: 'light', label: 'Light' },
  { id: 'medium-light', label: 'Medium-Light' },
  { id: 'medium', label: 'Medium' },
  { id: 'medium-dark', label: 'Medium-Dark' },
  { id: 'dark', label: 'Dark' },
];

export const EMOJI_COLS = 8;
export const SYMBOL_COLS = 8;

export const splitIntoRowsStatic = (items, cols, catId) => {
  const rows = [];
  for (let i = 0; i < items.length; i += cols) {
    rows.push({
      type: 'row',
      items: items.slice(i, i + cols),
      cols,
      catId,
      id: `${catId}-row-${i}`
    });
  }
  return rows;
};

let emojiDataCache = null;
let emojiMetaCache = null;
let emojiRowsCache = null;
let emojiSkinSupport = null;
let emojiLoadedLang = null;
let emojiLoadPromise = null;
let emojiLoadingLang = null;

const EMOJI_DATA_LOADERS = {
  en: () => import('emoji-picker-element-data/en/cldr/data.json'),
  zh: () => import('emoji-picker-element-data/zh/cldr/data.json'),
};

function normalizeEmojiLang(lang) {
  return String(lang || '').toLowerCase().startsWith('zh') ? 'zh' : 'en';
}

function buildEmojiData(rawData) {
  const grouped = {};
  const metaMap = {};
  const rowsCache = {};
  const skinSupport = new Map();

  rawData.forEach(item => {
    const groupId = GROUP_ID_MAP[item.group];
    if (!groupId) return;

    if (!grouped[groupId]) grouped[groupId] = [];
    const entry = {
      emoji: item.emoji,
      name: item.annotation,
      nameCn: item.annotation
    };
    grouped[groupId].push(entry);
    metaMap[item.emoji] = entry;

    if (item.skins && item.skins.length > 0) {
      skinSupport.set(item.emoji, item.skins.map(s => s.emoji));
    }
  });

  Object.keys(grouped).forEach(catId => {
    rowsCache[catId] = splitIntoRowsStatic(grouped[catId], EMOJI_COLS, catId);
  });

  emojiDataCache = grouped;
  emojiMetaCache = metaMap;
  emojiRowsCache = rowsCache;
  emojiSkinSupport = skinSupport;
}

/**
 * 按语言加载 emoji 数据；同一语言只加载一次，已加载其它语言时会重新构建。
 * @param {string} lang 当前界面语言
 * @returns {Promise<void>}
 */
export const ensureEmojiData = async (lang = 'en') => {
  const targetLang = normalizeEmojiLang(lang);
  if (emojiLoadedLang === targetLang && emojiDataCache) return;
  if (emojiLoadPromise && emojiLoadingLang === targetLang) return emojiLoadPromise;

  emojiLoadingLang = targetLang;
  emojiLoadPromise = EMOJI_DATA_LOADERS[targetLang]()
    .then(module => {
      if (emojiLoadingLang !== targetLang) return;
      buildEmojiData(module.default || []);
      emojiLoadedLang = targetLang;
    })
    .finally(() => {
      if (emojiLoadingLang === targetLang) {
        emojiLoadPromise = null;
        emojiLoadingLang = null;
      }
    });

  return emojiLoadPromise;
};

export const getEmojiDataCache = () => emojiDataCache;
export const getEmojiMetaCache = () => emojiMetaCache;
export const getEmojiRowsCache = () => emojiRowsCache;
export const getEmojiSkinSupport = () => emojiSkinSupport;
