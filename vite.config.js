import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import UnoCSS from 'unocss/vite'
import { resolve } from 'path'

const isDev = process.env.NODE_ENV === 'development'
const isTauriDebug = process.env.TAURI_DEBUG === 'true'

// 按包分组切分，避免所有依赖被压成单个 vendor（小窗口因此被迫加载列表/编辑器相关依赖）。
// 顺序敏感：越具体的规则越靠前，最后兜底进 vendor。
const VENDOR_CHUNK_GROUPS = [
  ['react', /node_modules[\\/](?:react|react-dom|scheduler)[\\/]/],
  ['virtuoso', /node_modules[\\/]react-virtuoso[\\/]/],
  ['dnd-kit', /node_modules[\\/]@dnd-kit[\\/]/],
  ['editor', /node_modules[\\/](?:@codemirror|@lezer)[\\/]/],
  ['i18n', /node_modules[\\/](?:i18next|react-i18next)[\\/]/],
  ['tauri', /node_modules[\\/]@tauri-apps[\\/]/],
  ['dompurify', /node_modules[\\/]dompurify[\\/]/],
  ['valtio', /node_modules[\\/]valtio[\\/]/],
  ['vendor', /node_modules[\\/]/],
]

// emoji 语言包必须保持独立异步 chunk（按语言各一份），
// 不能被 vendor 规则吞掉，否则会随静态依赖进首屏或被合并成一整块。
const EMOJI_DATA_PATTERN = /node_modules[\\/]emoji-picker-element-data[\\/]/

export default defineConfig({
  root: 'src',
  clearScreen: false,

  server: {
    port: 1421,
    strictPort: true,
    fs: {
      allow: [
        resolve(__dirname, '.'),
        resolve(__dirname, 'node_modules'),
        resolve(__dirname, 'src'),
      ],
    },
  },

  envPrefix: ['VITE_', 'TAURI_'],

  resolve: {
    preserveSymlinks: true,
    alias: {
      '@': resolve(__dirname, 'src'),
      '@shared': resolve(__dirname, 'src/shared'),
      '@windows': resolve(__dirname, 'src/windows'),
      'uno.css': 'virtual:uno.css',
    },
  },

  plugins: [
    UnoCSS({
      mode: 'global',
      inspector: false,
    }),
    react({
      babel: {
        plugins: [
          ['babel-plugin-react-compiler', {}],
        ],
      },
    }),
  ],

  build: {
    outDir: '../dist',
    // outDir 位于 root（src）之外，Vite 默认不会清空；显式开启避免旧产物被带进安装包。
    emptyOutDir: true,
    target: process.env.TAURI_PLATFORM === 'windows'
      ? 'chrome105'
      : 'safari16',

    minify: isDev || isTauriDebug ? false : 'esbuild',

    esbuild: isDev || isTauriDebug
      ? {}
      : {
          drop: ['debugger'],
          pure: ['console.log', 'console.info', 'console.debug'],
        },

    sourcemap: isDev || isTauriDebug,
    cssCodeSplit: true,

    rollupOptions: {
      input: {
          main: resolve(__dirname, 'src/windows/main/index.html'),
          settings: resolve(__dirname, 'src/windows/settings/index.html'),
          quickpaste: resolve(__dirname, 'src/windows/quickpaste/index.html'),
          textEditor: resolve(__dirname, 'src/windows/textEditor/index.html'),
          contextMenu: resolve(__dirname, 'src/plugins/context_menu/contextMenu.html'),
          inputDialog: resolve(__dirname, 'src/plugins/input_dialog/inputDialog.html'),
          pinImage: resolve(__dirname, 'src/windows/pinImage/pinImage.html'),
          preview: resolve(__dirname, 'src/windows/preview/index.html'),
      },

      output: {
        assetFileNames: 'assets/[name]-[hash][extname]',
        chunkFileNames: 'js/[name]-[hash].js',
        entryFileNames: 'js/[name]-[hash].js',

        manualChunks(id) {
          // shared 不再强制合并：交给 Rollup 按入口依赖图自动拆分，
          // 每个窗口只带自己用到的模块，避免小窗口被整块 shared 拖累。
          if (EMOJI_DATA_PATTERN.test(id)) return undefined
          for (const [chunkName, pattern] of VENDOR_CHUNK_GROUPS) {
            if (pattern.test(id)) return chunkName
          }
          return undefined
        },
      },
    },
  },
})
