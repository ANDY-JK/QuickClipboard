<h1 align="center">QuickClipboard</h1>

<p align="center">
  <strong>重新定义你的复制粘贴体验</strong><br>
  轻量 · 快速 · 智能 · 可定制
</p>

<div align="center">
  <img src="readme-assets/logo.png" alt="QuickClipboard Logo" width="120">
  <br><br>
  <a href="https://github.com/mosheng1/QuickClipboard/releases">
    <img src="https://img.shields.io/github/v/release/mosheng1/QuickClipboard?style=for-the-badge&label=Release&color=brightgreen" alt="Release">
  </a>
  <a href="https://github.com/mosheng1/QuickClipboard/releases">
    <img src="https://img.shields.io/github/downloads/mosheng1/QuickClipboard/total.svg?style=for-the-badge&color=blueviolet" alt="Downloads">
  </a>
  <a href="https://github.com/mosheng1/QuickClipboard/blob/main/LICENSE">
    <img src="https://img.shields.io/github/license/mosheng1/QuickClipboard.svg?style=for-the-badge&color=orange" alt="License">
  </a>
</div>

<div align="center">
  <b>简体中文</b> · <a href="i18n/README.en.md">English</a> · <a href="i18n/README.zh-TW.md">繁體中文</a> · <a href="i18n/README.ja.md">日本語</a> · <a href="i18n/README.ko.md">한국어</a>
</div>

---

## 简介

**QuickClipboard** 是一款跨平台剪贴板增强工具（现支持Windows，Android），基于 Tauri 2 + Rust + React 构建。它在你复制的那一刻就开始工作——自动记录文本、图片、富文本、文件，让你随时找回曾经复制过的任何内容。不只是记录，QuickClipboard 还集成了贴图、OCR 等能力，是日常办公效率的全面提升。

> 原生性能，内存占用低，启动即用，常驻系统托盘。

---

## 核心功能

| 模块                  | 功能                                                                                                                |
| --------------------- | ------------------------------------------------------------------------------------------------------------------- |
| 剪贴板管理          | 全类型记录（文本 / HTML / 图片 / 文件） · 智能去重 · 搜索筛选 · 虚拟列表 · 拖拽排序 / 置顶 · SQLite 持久化          |
| 内容预览            | 悬停预览文本 / HTML / 图片 / 文件列表 · Ctrl+滚轮滚动 / 缩放 · 多格式内容切换预览                                   |
| 快速粘贴            | 列表粘贴 · 数字快捷键 1-9 粘贴 · 纯文本 / 带格式粘贴 · 合并复制 / 合并粘贴 · 一次性粘贴 · 便捷粘贴窗口 · Win+V 支持 |
| 收藏与分组          | 收藏常用内容 · 自定义分组 / 图标 / 颜色 · 分组排序 · 批量移动到分组 · 快捷键切换分组                                |
| Emoji / 符号 / 图库 | Emoji 大全 · 符号大全 · 自定义图片 / GIF 图库 · 最近使用 · 拖拽 / 点击即用                                          |
| 贴图到屏幕          | 桌面置顶贴图 · 拖拽缩放 / 置顶 · 复制 / 另存为                                                                      |
| OCR 识别            | 图片 OCR · 一键提取并复制文字                                                                                        |
| 同步 / 传输         | 已移除（WebDAV 同步与局域网同步/传输不再提供）                                                                      |
| 边缘吸附与窗口      | 屏幕边缘自动隐藏 · 跟随光标唤出 · 窗口置顶 · 记忆位置 / 尺寸 · 标题栏方向切换                                       |
| 个性化              | 跟随系统 / 明暗主题 / 超级背景 · 多套主题风格 · 自定义背景 / 模糊 · 自定义字体 · 动画开关                           |
| 低内存模式          | 自动或手动切换轻量模式 · 即时恢复完整界面 · 告别内存焦虑（低占用模式占用仅~10MB）                                   |
| 专项后台优化        | 进入后台自动清理内存占用 · 暂停前端更新 · 降低系统资源占用（后台时内存占用~50MB）                                   |
| 数据管理            | ZIP 导入导出 · 备份恢复 · 自定义存储路径 · 数据迁移 / 合并 · 清空历史 · 便携模式                                    |
| 应用过滤            | 已过滤应用列表 · 剪贴板监听过滤 · 前台禁用所有功能 · 进程级规则                                                     |
| 系统集成            | 托盘常驻 · 开机自启动 · 管理员权限运行 · 启动通知                                                                    |

> 注意：WebDAV 同步、局域网同步/传输与自动更新功能已移除；如需获取新版本，请前往 [GitHub Releases](https://github.com/mosheng1/QuickClipboard/releases) 手动下载更新。

---

## 界面预览

<div align="center">

<table>
  <tr align="center">
    <td>
      <a href="readme-assets/display/浅色.png" target="_blank">
        <img src="readme-assets/display/浅色.png" alt="浅色主题">
      </a>
      <div><strong>浅色</strong></div>
    </td>
    <td>
      <a href="readme-assets/display/浅色手绘.png" target="_blank">
        <img src="readme-assets/display/浅色手绘.png" alt="浅色手绘主题">
      </a>
      <div><strong>浅色手绘</strong></div>
    </td>
    <td>
      <a href="readme-assets/display/暗色.png" target="_blank">
        <img src="readme-assets/display/暗色.png" alt="暗色主题">
      </a>
      <div><strong>暗色</strong></div>
    </td>
    <td>
      <a href="readme-assets/display/暗色经典.png" target="_blank">
        <img src="readme-assets/display/暗色经典.png" alt="暗色经典主题">
      </a>
      <div><strong>暗色经典</strong></div>
    </td>
    <td>
      <a href="readme-assets/display/暗色手绘.png" target="_blank">
        <img src="readme-assets/display/暗色手绘.png" alt="暗色手绘主题">
      </a>
      <div><strong>暗色手绘</strong></div>
    </td>
    <td>
      <a href="readme-assets/display/自定义背景.png" target="_blank">
        <img src="readme-assets/display/自定义背景.png" alt="自定义背景">
      </a>
      <div><strong>自定义背景</strong></div>
    </td>
  </tr>
</table>

<table>
  <tr align="center">
    <td>
      <a href="readme-assets/display/设置.png" target="_blank">
        <img src="readme-assets/display/设置.png" alt="设置界面">
      </a>
      <div><strong>设置</strong></div>
    </td>
    <td>
      <a href="readme-assets/display/表情符号页.png" target="_blank">
        <img src="readme-assets/display/表情符号页.png" alt="表情符号页">
      </a>
      <div><strong>表情符号页</strong></div>
    </td>
    <td>
      <a href="readme-assets/display/图库页.png" target="_blank">
        <img src="readme-assets/display/图库页.png" alt="图库页">
      </a>
      <div><strong>图库页</strong></div>
    </td>
  </tr>
</table>

<table>
  <tr align="center">
    <td>
      <a href="readme-assets/display/内容预览.gif" target="_blank">
        <img src="readme-assets/display/内容预览.gif" alt="内容预览">
      </a>
      <div><strong>内容预览</strong></div>
    </td>
  </tr>
</table>

</div>

---

## 系统要求

- Windows 10 / 11 (x64)  

---

## 下载方式（v0.5.0）

| 类型                                                      |                       说明 |                                                        下载量                                                        | 链接                                                                                                                                                                                            |
| --------------------------------------------------------- | -------------------------: | :------------------------------------------------------------------------------------------------------------------: | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **NSIS 安装包**<br>`QuickClipboard_0.5.0_x64-setup.exe` | 推荐安装方式，支持自动卸载 | ![下载量](https://img.shields.io/github/downloads/mosheng1/QuickClipboard/QuickClipboard_0.5.0_x64-setup.exe?label=) | [![下载 NSIS](https://img.shields.io/badge/下载-NSIS安装包-blue?style=for-the-badge)](https://github.com/mosheng1/QuickClipboard/releases/download/v0.5.0/QuickClipboard_0.5.0_x64-setup.exe) |
| **绿色版**<br>`QuickClipboard_0.5.0.exe`                |       免安装，双击即可使用 |      ![下载量](https://img.shields.io/github/downloads/mosheng1/QuickClipboard/QuickClipboard_0.5.0.exe?label=)      | [![下载绿色版](https://img.shields.io/badge/下载-绿色版-orange?style=for-the-badge)](https://github.com/mosheng1/QuickClipboard/releases/download/v0.5.0/QuickClipboard_0.5.0.exe)            |
| **便携版**<br>`QuickClipboard_0.5.0_portable.exe`       |      更适合放U盘或移动使用 | ![下载量](https://img.shields.io/github/downloads/mosheng1/QuickClipboard/QuickClipboard_0.5.0_portable.exe?label=)  | [![下载便携版](https://img.shields.io/badge/下载-便携版-green?style=for-the-badge)](https://github.com/mosheng1/QuickClipboard/releases/download/v0.5.0/QuickClipboard_0.5.0_portable.exe)    |
| **安卓版**<br>`QuickClipboard_Android_v1.0.4.apk`       |    适用于 Android 设备安装 | ![下载量](https://img.shields.io/github/downloads/mosheng1/QuickClipboard/QuickClipboard_Android_v1.0.4.apk?label=)  | [![下载安卓版](https://img.shields.io/badge/下载-安卓版-success?style=for-the-badge)](https://github.com/mosheng1/QuickClipboard/releases/download/v0.4.0/QuickClipboard_Android_v1.0.4.apk)  |

---


## 开发与构建

### 环境依赖

- Node.js ≥ 16  
- Rust ≥ 1.70  
- Tauri CLI ≥ 2.0

### 常用命令

```bash
# 安装依赖
npm install

# 开发模式
npm run tauri dev

# 构建发行版
npm run tauri:build

# 代码检查与测试
npm run check                  # Rust 编译检查
npm run clippy                 # Rust lint
npm run test:rust             # Rust 单元测试
```

## 许可证

 本项目基于 [Apache License 2.0](LICENSE) 开源。

## 来源与许可

本项目是基于原版 [QuickClipboard](https://github.com/mosheng1/QuickClipboard) 二次开发的独立维护版本。原项目由 **MoSheng** 创建，并以 [Apache License 2.0](LICENSE) 许可发布。

- 原仓库：<https://github.com/mosheng1/QuickClipboard>
- 原版权声明：Copyright 2025 MoSheng
- 维护者：**ANDY-JK**，衍生仓库：<https://github.com/ANDY-JK/QuickClipboard>
- 许可证：[Apache License 2.0](LICENSE)，另见 [NOTICE](NOTICE)
- 本仓库为独立维护的衍生版本，**并非原作者的官方发布**；原项目的版权声明与许可条款继续适用。

原仓库的徽章、下载链接、应用内 GitHub 入口与 Discussions 链接均保留，用于标注上游来源。
