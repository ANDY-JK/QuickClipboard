<h1 align="center">QuickClipboard</h1>

<p align="center">
  <strong>Redefine Your Copy & Paste Experience</strong><br>
  Lightweight · Fast · Smart · Customizable
</p>

<div align="center">
  <img src="../readme-assets/logo.png" alt="QuickClipboard Logo" width="120">
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
  <a href="../README.md">简体中文</a> · <b>English</b> · <a href="README.zh-TW.md">繁體中文</a> · <a href="README.ja.md">日本語</a> · <a href="README.ko.md">한국어</a>
</div>

---

## Introduction

**QuickClipboard** is a cross-platform clipboard enhancement tool (currently supports Windows and Android), built with Tauri 2 + Rust + React. It starts working the moment you copy — automatically recording text, images, rich text, and files, so you can always retrieve anything you've ever copied. Beyond just recording, QuickClipboard integrates image pinning, OCR, and more, delivering a comprehensive productivity boost for daily work.

> Native performance, low memory footprint, ready to use on launch, lives in your system tray.

---

## Core Features

| Module                  | Features                                                                                                            |
| ----------------------- | ------------------------------------------------------------------------------------------------------------------- |
| Clipboard Manager       | All-type recording (text / HTML / images / files) · Smart dedup · Search & filter · Virtual list · Drag sort / pin · SQLite persistence |
| Content Preview         | Hover preview for text / HTML / images / file lists · Ctrl+scroll to scroll / zoom · Multi-format content switching |
| Quick Paste             | Paste from list · Number keys 1-9 to paste · Plain text / formatted paste · Merge copy / merge paste · One-shot paste · Quick paste window · Win+V support |
| Favorites & Groups      | Save favorites · Custom groups / icons / colors · Group sorting · Batch move to groups · Keyboard group switching   |
| Emoji / Symbols / Gallery | Complete Emoji set · Symbol library · Custom image / GIF gallery · Recently used · Drag or click to use            |
| Pin to Screen           | Desktop pinned images · Drag to resize / pin · Copy / save as                                                        |
| OCR Recognition         | Image OCR · One-click extract and copy text                                                                          |
| Sync / Transfer         | Removed (WebDAV sync and LAN sync/transfer are no longer available)                                                |
| Edge Snap & Window      | Auto-hide at screen edge · Summon by cursor · Pin window · Remember position / size · Title bar orientation switch  |
| Personalization         | Follow system / Light-Dark theme / Super background · Multiple theme styles · Custom background / blur · Custom font · Animation toggle |
| Low Memory Mode         | Auto or manual switch to lightweight mode · Instant full-UI recovery · Say goodbye to memory anxiety (~10MB in low-memory mode) |
| Background Optimization | Auto-clean memory when in background · Pause frontend updates · Reduce system resource usage (~50MB in background)   |
| Data Management         | ZIP import / export · Backup & restore · Custom storage path · Data migration / merge · Clear history · Portable mode |
| App Filtering           | Filtered apps list · Clipboard monitoring filtering · Disable all features in foreground · Process-level rules |
| System Integration      | System tray resident · Auto-start on boot · Run as administrator · Startup notification                            |

> Note: WebDAV sync, LAN sync/transfer, and auto-update have been removed. To get new versions, download updates manually from [GitHub Releases](https://github.com/mosheng1/QuickClipboard/releases).

---

## UI Preview

<div align="center">

<table>
  <tr align="center">
    <td>
      <a href="../readme-assets/display/浅色.png" target="_blank">
        <img src="../readme-assets/display/浅色.png" alt="浅色主题">
      </a>
      <div><strong>Light</strong></div>
    </td>
    <td>
      <a href="../readme-assets/display/浅色手绘.png" target="_blank">
        <img src="../readme-assets/display/浅色手绘.png" alt="浅色手绘主题">
      </a>
      <div><strong>Light Sketch</strong></div>
    </td>
    <td>
      <a href="../readme-assets/display/暗色.png" target="_blank">
        <img src="../readme-assets/display/暗色.png" alt="暗色主题">
      </a>
      <div><strong>Dark</strong></div>
    </td>
    <td>
      <a href="../readme-assets/display/暗色经典.png" target="_blank">
        <img src="../readme-assets/display/暗色经典.png" alt="暗色经典主题">
      </a>
      <div><strong>Dark Classic</strong></div>
    </td>
    <td>
      <a href="../readme-assets/display/暗色手绘.png" target="_blank">
        <img src="../readme-assets/display/暗色手绘.png" alt="暗色手绘主题">
      </a>
      <div><strong>Dark Sketch</strong></div>
    </td>
    <td>
      <a href="../readme-assets/display/自定义背景.png" target="_blank">
        <img src="../readme-assets/display/自定义背景.png" alt="自定义背景">
      </a>
      <div><strong>Custom Background</strong></div>
    </td>
  </tr>
</table>

<table>
  <tr align="center">
    <td>
      <a href="../readme-assets/display/设置.png" target="_blank">
        <img src="../readme-assets/display/设置.png" alt="设置界面">
      </a>
      <div><strong>Settings</strong></div>
    </td>
    <td>
      <a href="../readme-assets/display/表情符号页.png" target="_blank">
        <img src="../readme-assets/display/表情符号页.png" alt="表情符号页">
      </a>
      <div><strong>Emoji Picker</strong></div>
    </td>
    <td>
      <a href="../readme-assets/display/图库页.png" target="_blank">
        <img src="../readme-assets/display/图库页.png" alt="图库页">
      </a>
      <div><strong>Gallery</strong></div>
    </td>
  </tr>
</table>

<table>
  <tr align="center">
    <td>
      <a href="../readme-assets/display/内容预览.gif" target="_blank">
        <img src="../readme-assets/display/内容预览.gif" alt="内容预览">
      </a>
      <div><strong>Content Preview</strong></div>
    </td>
  </tr>
</table>

</div>

---

## System Requirements

- Windows 10 / 11 (x64)

---

## Download (v0.4.0)

| Type                                                        |                   Description |                                                      Downloads                                                      | Link                                                                                                                                                                                          |
| ----------------------------------------------------------- | ----------------------------: | :-----------------------------------------------------------------------------------------------------------------: | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **NSIS Installer**<br>`QuickClipboard_0.5.0_x64-setup.exe`  |    Recommended; supports auto-uninstall | ![Downloads](https://img.shields.io/github/downloads/mosheng1/QuickClipboard/QuickClipboard_0.5.0_x64-setup.exe?label=) | [![Download NSIS](https://img.shields.io/badge/Download-NSIS%20Installer-blue?style=for-the-badge)](https://github.com/mosheng1/QuickClipboard/releases/download/v0.5.0/QuickClipboard_0.5.0_x64-setup.exe) |
| **Portable (Standalone)**<br>`QuickClipboard_0.5.0.exe`      |     No installation required  | ![Downloads](https://img.shields.io/github/downloads/mosheng1/QuickClipboard/QuickClipboard_0.5.0.exe?label=)       | [![Download Portable](https://img.shields.io/badge/Download-Portable-orange?style=for-the-badge)](https://github.com/mosheng1/QuickClipboard/releases/download/v0.5.0/QuickClipboard_0.5.0.exe)    |
| **Portable (USB)**<br>`QuickClipboard_0.5.0_portable.exe`    | Ideal for USB drives & mobile use | ![Downloads](https://img.shields.io/github/downloads/mosheng1/QuickClipboard/QuickClipboard_0.5.0_portable.exe?label=) | [![Download USB](https://img.shields.io/badge/Download-Portable%20USB-green?style=for-the-badge)](https://github.com/mosheng1/QuickClipboard/releases/download/v0.5.0/QuickClipboard_0.5.0_portable.exe) |
| **Android APK**<br>`QuickClipboard_Android_v1.0.4.apk`       |        For Android devices    | ![Downloads](https://img.shields.io/github/downloads/mosheng1/QuickClipboard/QuickClipboard_Android_v1.0.4.apk?label=) | [![Download Android](https://img.shields.io/badge/Download-Android-success?style=for-the-badge)](https://github.com/mosheng1/QuickClipboard/releases/download/v0.4.0/QuickClipboard_Android_v1.0.4.apk) |

---


## Development & Build

### Prerequisites

- Node.js ≥ 16  
- Rust ≥ 1.70  
- Tauri CLI ≥ 2.0

### Common Commands

```bash
# Install dependencies
npm install

# Development mode
npm run tauri dev

# Build release
npm run tauri:build
```

## License

This project is licensed under the [Apache License 2.0](../LICENSE).

## Provenance and License

This project is an independently maintained derivative based on the original [QuickClipboard](https://github.com/mosheng1/QuickClipboard), originally created by **MoSheng** and released under the [Apache License 2.0](../LICENSE).

- Original repository: <https://github.com/mosheng1/QuickClipboard>
- Original copyright notice: Copyright 2025 MoSheng
- License: [Apache License 2.0](../LICENSE), see also [NOTICE](../NOTICE)
- This repository is an independently maintained derivative and is **not an official release of the original author**. The original copyright notice and license terms continue to apply.

The original repository's badges, download links, in-app GitHub entry and Discussions link are retained to credit the upstream project.
