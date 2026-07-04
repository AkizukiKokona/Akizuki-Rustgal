<div align="center">

# Akizuki\*Rustgal

**纯 Rust 打造的视觉小说引擎 — 编译时检查、自定义 DSL、跨平台运行**

[![License](https://img.shields.io/badge/license-MIT-blue.svg)](#许可证)
[![Rust](https://img.shields.io/badge/rust-1.92+-orange.svg)](https://www.rust-lang.org)
[![Version](https://img.shields.io/badge/version-1.0.0-success.svg)](#)
[![Platform](https://img.shields.io/badge/platform-Windows%20%7C%20Linux%20%7C%20macOS-lightgrey.svg)](#下载与安装)
[![PRs Welcome](https://img.shields.io/badge/PRs-welcome-brightgreen.svg)](#贡献)

</div>

---

Akizuki\*Rustgal 是一个从零开始、**100% 纯 Rust** 实现的视觉小说引擎。从词法分析器、语法解析器、类型检查器到虚拟机、渲染器、编辑器——每一行代码都是 Rust。不依赖 Lua、不依赖 Python、不依赖任何脚本运行时。

引擎配备了一套自定义剧本语言（`.akrs`），支持编译时资源检查、分支选择、变量系统、过渡动画等视觉小说核心功能，同时附带可视化编辑器和三平台自动构建流水线。

## 为什么选择纯 Rust？

| 特性 | 说明 |
|------|------|
| **内存安全** | 所有权系统在编译期消除空指针、悬垂引用、数据竞争 |
| **零成本抽象** | 泛型和 trait 不引入运行时开销，性能媲美 C/C++ |
| **无 GC 停顿** | 没有垃圾回收器，游戏帧率稳定可预测 |
| **Fearless concurrency** | 编译器保证线程安全，多线程开发无需提心吊胆 |
| **跨平台编译** | 一套代码编译到 Windows / Linux / macOS / WASM |

## 功能特性

- **自定义剧本语言** — 专为视觉小说设计的 `.akrs` DSL，支持对话、旁白、背景切换、立绘上场/下场、分支选择、变量、条件跳转
- **编译时检查** — 在运行之前就发现错误：未定义的章节跳转、重复的节名、缺失的资源引用、舞台角色超限
- **12 种过渡效果** — `fade`、`dissolve`、`slide_left/right/up/down`、`wipe_left/right`、`blur`、`fade_black`、`fade_white`、`instant`
- **多分辨率自适应** — 所有立绘位置使用百分比坐标（0.0–1.0），在 1080p / 1440p / 4K 下布局比例一致
- **可视化编辑器** — 基于 egui 的剧本编辑器，支持语法高亮、立绘预览、实时调参
- **存档系统** — 多槽位存档/读档，支持崩溃恢复
- **热重载** — 修改剧本后自动重新编译，无需重启游戏
- **快进模式** — 支持仅文本快进和包含语音快进两种模式
- **设置系统** — 文字速度、音量、分辨率、全屏等可配置项
- **多语言翻译系统** — 原文剧本不修改，翻译文件与原文一一对应；支持章节标题、对话、旁白、分支选项、角色名翻译；编辑器内置对照翻译模式（原文-译文并排，原文只读）；CLI 一键生成翻译骨架；运行时设置页实时切换语言
- **编辑器内置打包** — GUI 勾选 Windows/Linux/macOS 平台，一键 `cargo build --release --target`，自动收集产物到 `build/` 目录
- **编辑器游戏预览** — 点击按钮即可在独立窗口启动游戏预览，编辑器可继续编辑，互不阻塞

## 快速开始

### 下载与安装

前往 [Releases](https://github.com/AkizukiKokona/Akizuki-Rustgal/releases) 下载最新版本：

- **Windows**：下载 `.zip`，解压后双击 `启动游戏.bat`
- **Linux / macOS**：下载 `.tar.gz`，解压后运行 `./启动游戏.sh`

### 从源码构建

```bash
# 克隆仓库（含 macroquad 补丁子模块）
git clone https://github.com/AkizukiKokona/Akizuki-Rustgal.git
cd Akizuki-Rustgal

# 构建游戏
cargo build --release -p akrs-game

# 构建编辑器
cargo build --release -p akrs-editor

# 构建命令行工具
cargo build --release -p akrs-cli

# 运行示例游戏
cargo run --release -p akrs-game

# 运行编辑器
cargo run --release -p akrs-editor
```

> **Rust 版本要求**：1.92.0+（使用 Rust 2024 Edition）
>
> **Linux 额外依赖**：`libxcb-render-util0-dev libxcb-shape0-dev libxcb-xfixes0-dev libxcb-xkb-dev libasound2-dev`

## 剧本语言速览

```ruby
# 夏日祭的尾声

-- 背景：夏日祭夜晚
@bg bg1 with fade

-- 立绘：心夏（身体1）居中
+ 心夏 (kokonabody1) 居中

心夏: "引航者，今天晚上玩得开心吗？"

引航者: "不错不错，可惜今晚是夏日祭最后一天了。"

-- 旁白
"心夏低下了头，不过不到一秒她又把头抬起来了。"

-- 分支选项
? "引航者要对心夏说什么？"
| "我有个小礼物想送给心夏" -> 分支A
| "我给心夏买了一套衣服" -> 分支B
?

-- 变量与条件
$好感度 = 10
if 好感度 >= 10 then
  心夏: "嘿嘿，也许我有呢？"
end

-- 切换背景 + CG
@bg cg with fade

~~
```

### 命令行工具

```bash
# 检查剧本是否有错误（含资源引用检查）
akrs check scripts/demo.akrs assets/

# 文本模式运行（无需图形界面）
akrs run scripts/demo.akrs

# 生成翻译骨架文件（扫描剧本提取可翻译文本）
akrs translate init scripts/demo.akrs ja-JP > assets/scripts/languages/ja-JP.json
```

## 项目架构

```
akrs-core      DSL 核心：词法分析 → 语法解析 → AST → 类型检查 → 虚拟机
akrs-macros    过程宏：编译时将 .akrs 脚本嵌入 Rust 代码
akrs-runtime   运行时引擎：场景状态、过渡动画、存档、热重载、设置
akrs-render    渲染层：基于 macroquad 的 2D 渲染器
akrs-editor    可视化编辑器：基于 egui 的剧本编辑与预览工具
akrs-pack      打包工具：将游戏资源打包为可分发包
akrs-cli       命令行工具：检查、运行、打包
akrs-game      游戏启动器：读取剧本并启动图形界面
```

## 技术栈

本项目完全使用 Rust 编写，依赖以下优秀的 Rust 生态项目：

| 依赖 | 用途 |
|------|------|
| [**macroquad**](https://github.com/not-fl3/macroquad) | 跨平台 2D 游戏引擎，提供渲染、输入、音频、窗口管理（本项目使用本地补丁版本） |
| [**egui**](https://github.com/emilk/egui) / eframe | 即时模式 GUI 框架，用于编辑器界面 |
| [**serde**](https://github.com/serde-rs/serde) / serde_json | 序列化框架，用于存档和设置持久化 |
| [**syn**](https://github.com/dtolnay/syn) / quote / proc-macro2 | 过程宏工具链，用于编译时脚本嵌入 |
| [**notify**](https://github.com/notify-rs/notify) | 文件系统监听，用于热重载 |
| [**codespan-reporting**](https://github.com/brendanzab/codespan) | 编译错误诊断信息格式化 |

## 文档

| 文档 | 说明 |
|------|------|
| [剧本语言规范](docs/剧本语言规范.md) | `.akrs` 剧本语言的完整语法参考，涵盖所有指令、变量、流程控制和编译时检查规则 |
| [立绘摆放语法](docs/立绘摆放语法.md) | 立绘位置、大小、过渡效果的详细说明与编辑器预览指南 |
| [翻译功能使用说明](docs/翻译功能使用说明.md)（[EN](docs/translation-guide.en.md) / [JA](docs/translation-guide.ja.md) / [ZH-TW](docs/translation-guide.zh-TW.md)） | 多语言翻译系统完整指南：翻译文件格式、CLI 骨架生成、编辑器对照翻译模式、运行时语言切换 |
| [DSL 规范（交互式 HTML）](docs/dsl-specification.html) | 带语法高亮和侧边导航的网页版语法文档 |

## 编辑器功能

### 游戏预览

点击工具栏「预览游戏」按钮，编辑器会自动保存当前脚本并通过 `cargo run --release -p akrs-game` 启动独立游戏窗口。预览窗口与编辑器互不阻塞，可一边编辑一边预览。子进程的标准输出/错误直接透传到终端，便于调试崩溃和日志。

### 三端打包

点击工具栏「打包」按钮打开打包面板：

1. 勾选目标平台（Windows / Linux / macOS，可多选）
2. 点击「开始打包」，编辑器会依次执行 `cargo build --release --target <平台>`
3. 构建完成后，可执行文件和资源自动收集到 `build/akrs-game-<平台>/` 目录
4. 点击「打开目录」在文件管理器中查看产物

**打包健壮性保证：**

- **动态产物名** — 从 `Cargo.toml` 的 `[[bin]]` 段动态读取二进制目标名，无需硬编码
- **失败不阻塞** — 单个平台构建失败不影响其他平台，全部完成后汇总成功/失败数及失败原因
- **脚本快照** — 打包开始前锁定 `scripts/` 目录快照，打包期间用户修改脚本不影响产物内容，完成后自动清理
- **日志自动跟底** — 构建日志自动滚动到最新行，用户向上滚动时暂停跟底，避免干扰阅读

> **前提条件**：需要本地安装 Rust 工具链。如果未检测到 cargo，编辑器会弹出引导窗口，提供清华源加速安装命令。

### 清华源配置（推荐）

国内用户安装 Rust 时建议使用清华镜像加速：

```bash
# 设置清华源
export RUSTUP_DIST_SERVER=https://mirrors.tuna.tsinghua.edu.cn/rustup

# 安装 Rust（Linux/macOS）
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

或访问 [rustup.rs](https://rustup.rs) 下载安装器。

## 开发

### 项目结构

```
.
├── crates/              各功能模块（每个子目录一个 crate）
├── assets/              游戏资源（背景、立绘、字体、图标）
├── scripts/             示例剧本
├── docs/                文档
├── patches/macroquad/   macroquad 本地补丁
├── .github/workflows/   CI/CD 流水线（开发构建 + 编辑器构建 + 发行版构建）
└── Cargo.toml           Workspace 根配置
```

### 构建配置

Release 构建已优化为最高性能：

```toml
[profile.release]
opt-level = 3
lto = true           # 全链接时优化
codegen-units = 1    # 单代码生成单元，最大化优化
strip = true         # 去除调试符号
```

### CI/CD

- **dev-build.yml** — 开发分支推送触发三平台构建测试
- **editor-build.yml** — 编辑器独立构建
- **release.yml** — 推送 `v*` tag 时自动构建并创建 GitHub Release

## 贡献

欢迎提交 Issue 和 Pull Request！

1. Fork 本仓库
2. 创建功能分支（`git checkout -b feature/amazing-feature`）
3. 提交更改（`git commit -m 'Add amazing feature'`）
4. 推送到分支（`git push origin feature/amazing-feature`）
5. 创建 Pull Request

## 许可证

本项目基于 [MIT 许可证](LICENSE) 开源。
