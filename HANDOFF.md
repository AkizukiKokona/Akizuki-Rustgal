# Akizuki\*Rustgal 项目交接文档

> **阅读对象**：接手此项目的 AI 智能体或人类开发者。本文档包含上手所需的全部信息，无需回溯历史对话。

---

## 1. 项目概述

Akizuki\*Rustgal 是一个用 Rust 编写的视觉小说引擎，包含自研 DSL（`.akrs` 格式）、编译期语法检查、运行时虚拟机、macroquad 图形渲染器、egui 编辑器、命令行工具和打包器。

- **语言**：Rust（edition 2024）
- **工具链**：固定 Rust 1.92.0（通过仓库根目录 `rust-toolchain.toml` 锁定；系统已安装，路径 `/root/.cargo/bin/cargo`）
- **许可证**：MIT
- **版本**：1.0.0
- **非 Git 仓库**：当前项目目录无 `.git`，无版本历史

---

## 2. 环境准备

### 2.1 Rust 工具链

仓库根目录新增 `rust-toolchain.toml`，自动锁定 Rust 1.92.0 并附带 `rustfmt`、`clippy` 组件。进入项目目录后 `rustup` 会自动安装/切换到该工具链，无需手动 `source`。

```bash
# 进入项目目录后，rustup 会自动使用 1.92.0
cd /workspace
rustc --version   # 应显示 1.92.0
```

**关键约束**：必须使用 Rust 1.92.0，edition 已升级到 2024。`eframe` 依赖仍禁用 `default-features`（仅启用 `glow` 和 `default_fonts`）——这是有意的设计选择（本项目不需要 accesskit 无障碍栈），原先针对 Rust 1.75 的 atspi/zbus 不兼容问题已不再适用。

### 2.2 Windows 交叉编译

已配置 MinGW 交叉编译工具链：

```bash
# .cargo/config.toml 已配置链接器
[target.x86_64-pc-windows-gnu]
linker = "x86_64-w64-mingw32-gcc"
ar = "x86_64-w64-mingw32-gcc-ar"
```

编译 Windows 版本：
```bash
cd /workspace
cargo build --workspace --release --target x86_64-pc-windows-gnu
```

### 2.3 系统依赖（Linux 开发环境）

- `libgl` / `libegl`（macroquad 依赖）
- `libasound2`（音频播放）
- 中文字体（开发调试用）：系统已安装 `Noto Sans CJK SC` 和 `WenQuanYi MicroHei`

---

## 3. 项目结构

```
akizuki-rustgal/
├── Cargo.toml              # workspace 根配置 + [patch.crates-io]，edition = "2024"
├── Cargo.lock
├── rust-toolchain.toml     # 锁定 Rust 1.92.0（含 rustfmt + clippy）
├── .cargo/config.toml      # Windows 交叉编译链接器
├── kokona.png              # 原始窗口图标素材
│
├── crates/
│   ├── akrs-core/          # DSL 核心：lexer/parser/AST/checker/VM/diagnostic
│   ├── akrs-macros/        # 编译期 DSL 检查 proc-macro
│   ├── akrs-runtime/       # 运行时引擎：Engine/GameState/SaveLoad/Settings/Transition/HotReload
│   ├── akrs-render/        # macroquad 图形渲染器（renderer.rs 是最大文件，~1780 行）
│   ├── akrs-editor/        # egui 可视化编辑器
│   ├── akrs-pack/          # 打包工具
│   ├── akrs-cli/           # 命令行工具（bin name: akrs）
│   └── akrs-game/          # 图形启动器（bin name: akrs-game）
│
├── patches/
│   └── macroquad/          # macroquad 0.3.26 本地补丁（仅改 text.rs 一处）
│
├── scripts/
│   └── demo.akrs           # 示例剧本（展示全部 DSL 特性）
│
├── assets/
│   ├── fonts/
│   │   └── NotoSansSC-Regular-subset.ttf  # 精简中文字体（~11MB）
│   ├── icon_kokona_16.bin  # 16x16 raw RGBA（窗口图标）
│   ├── icon_kokona_32.bin  # 32x32 raw RGBA
│   ├── icon_kokona_64.bin  # 64x64 raw RGBA
│   ├── icon.png            # 程序化生成的备用图标 PNG
│   └── icon_256.png
│
├── docs/
│   └── dsl-specification.html  # .akrs DSL 语言规范（1299 行）
│
├── examples/               # 空目录
├── saves/                  # 运行时存档目录
└── vendor/                 # 空目录
```

### 3.1 Crate 依赖关系

```
akrs-core
  ↑
akrs-macros (proc-macro)     akrs-runtime
                                ↑
              ┌─────────────────┼──────────────┐
        akrs-render        akrs-editor      akrs-pack
              ↑                                ↑
        akrs-game          akrs-cli ──────────┘
```

| Crate | 类型 | 说明 |
|-------|------|------|
| `akrs-core` | lib | DSL 工具箱：lexer、parser、AST、type checker、VM、diagnostic |
| `akrs-macros` | proc-macro | 编译期 DSL 语法检查（`check_script!` 宏） |
| `akrs-runtime` | lib | 运行时引擎，feature `hot-reload`（默认启用） |
| `akrs-render` | lib | macroquad 图形渲染，字体加载，UI 交互 |
| `akrs-editor` | lib | egui 可视化编辑器 |
| `akrs-pack` | lib | 打包工具（复制二进制+assets，生成启动脚本） |
| `akrs-cli` | bin (`akrs`) | 命令行：check / run / pack / migrate / help |
| `akrs-game` | bin (`akrs-game`) | 图形启动器，默认读 `scripts/demo.akrs` |

---

## 4. 常用命令

```bash
# 工具链由 rust-toolchain.toml 自动管理，无需手动 source
# 进入项目目录即可
cd /workspace

# 编译（debug）
cargo build --workspace

# 编译（release）
cargo build --workspace --release

# 运行全部测试（64 个测试，6 个忽略）
cargo test --workspace

# Windows 交叉编译
cargo build --workspace --release --target x86_64-pc-windows-gnu

# 只检查某个 crate
cargo check -p akrs-render

# 运行命令行工具
cargo run -p akrs-cli -- check scripts/demo.akrs
cargo run -p akrs-cli -- run scripts/demo.akrs
cargo run -p akrs-cli -- migrate input.rpy output.akrs
cargo run -p akrs-cli -- pack --help
```

---

## 5. macroquad 本地补丁（关键！）

### 5.1 为什么需要补丁

macroquad 0.3.26 的 `src/text.rs` 中有一段代码：

```rust
if metrics.advance_height != 0.0 {
    panic!("Vertical fonts are not supported");
}
```

CJK 字体（NotoSansSC、微软雅黑、文泉驿等）普遍包含 vertical metrics 表，导致 fontdue 返回非零的 `advance_height`，触发 panic 崩溃。

### 5.2 补丁内容

补丁位于 `patches/macroquad/src/text.rs`，删除了上述 panic 检查，改为注释说明：

```rust
// CJK fonts often include vertical metrics tables, which cause
// fontdue to report a non-zero advance_height.  The font is still
// perfectly usable for horizontal layout, so we simply ignore this
// check and continue caching the glyph normally.
```

### 5.3 补丁生效方式

`Cargo.toml` 根配置中有：

```toml
[patch.crates-io]
macroquad = { path = "patches/macroquad" }
```

这会让所有 crate 使用本地 `patches/macroquad` 而非 crates.io 上的版本。

### 5.4 注意事项

- **不要丢失此补丁**。如果移除 `[patch.crates-io]` 或删除 `patches/` 目录，CJK 字体加载会 panic。
- 补丁中只修改了 `src/text.rs` 一处，其余文件与上游 0.3.26 完全一致。
- 如果升级 macroquad 版本，需要重新应用此补丁。

---

## 6. 字体系统

### 6.1 字体加载流程

`renderer.rs` 中的 `load_font_with_fallback()` 函数返回 `(primary_font, fallback_font)`：

1. **嵌入字体**（最高优先级）：通过 `include_bytes!("../../../assets/fonts/NotoSansSC-Regular-subset.ttf")` 编译时嵌入，用 `load_ttf_font_from_bytes()` 加载
2. **运行时文件**：读取 `assets/fonts/NotoSansSC-Regular-subset.ttf`
3. **系统字体**（按平台）：
   - Windows: `msyh.ttc` → `msyh.ttf` → `simsun.ttc` → `simhei.ttf`
   - macOS: `PingFang.ttc` → `STHeiti` → `Hiragino Sans GB`
   - Linux: Noto Sans CJK SC → 文泉驿微米黑 → 文泉驿正黑

加载后用 `measure_text("世", ...)` 验证字体是否可用（宽度 > 0）。

### 6.2 字符级回退

`draw_text_f()` 函数实现字符级回退：
1. 检查主字体是否能渲染整串文本
2. 如果不能，检查回退字体（thread-local `FALLBACK_FONT`）
3. 如果两者都无法渲染某个字符，将该字符替换为 `□`（U+25A1）

### 6.3 字体精简

`NotoSansSC-Regular-subset.ttf` 是从完整 NotoSansCJKsc-Regular.otf（16MB）中用 fonttools 精简而来（~11MB），包含：
- ASCII（0x20-0x7F）
- CJK 统一汉字（0x4E00-0x9FFF）
- CJK 兼容表意文字（0xF900-0xFAFF）
- CJK 标点符号（0x3000-0x3040）
- 全角形式（0xFF00-0xFFEF）
- 游戏特殊字符（▼●○■□◆◇★☆♪♫→←↑↓ 等）

重新精简字体的命令：
```bash
python3 -c "
from fontTools.subset import Subsetter, Options
import fontTools.ttLib

font = fontTools.ttLib.TTFont('NotoSansCJKsc-Regular.otf')
subsetter = Subsetter()
# 填充所需字符集...
subsetter.subset(font)
font.save('NotoSansSC-Regular-subset.ttf')
"
```

### 6.4 关键 API 选择

- 使用 `load_ttf_font_from_bytes()`（同步）而非 `load_ttf_font()`（async），避免文件路径和异步问题
- 使用 `draw_text_ex()` 而非 `draw_text()`，后者不支持自定义字体
- 使用 `measure_text(text, Some(font), ...)` 而非 `measure_text(text, None, ...)`

---

## 7. .akrs DSL 语言

### 7.1 语法速览

```
# 章节名                    章节标记（跳转目标）
-- 注释                     单行注释
@bg 名称 with 过渡效果       切换背景
@music 名称                 播放背景音乐
+ 角色名 enters from 方向 with 过渡效果   角色登场
- 角色名                    角色退场
"旁白文本"                   旁白
角色名: "对话"               角色对话
角色名 (表情): "对话"        带表情的角色对话
$变量 = 值                   变量赋值
$变量 += 值                  变量运算
if $条件 ... else ... end   条件分支
-> 目标章节                  跳转
=> 子例程                    拜访（调用后返回）
<=                          从子例程返回
? "提示文本"                 选项块开始
| "选项文本"                  选项分支
    选项内代码...
    -> 跳转目标
?                           选项块结束
~~                          故事结束
```

### 7.2 完整规范

详细 DSL 规范见 `docs/dsl-specification.html`（1299 行 HTML 文档）。

### 7.3 示例剧本

`scripts/demo.akrs`（108 行）展示了全部 DSL 特性，包括章节、背景切换、角色进退场、对话、变量、条件分支、选项、子例程调用、多结局。

---

## 8. 渲染器架构（renderer.rs）

### 8.1 核心状态

- `UiMode` 枚举：`Normal` / `SaveMenu` / `LoadMenu` / `SettingsMenu` / `AutoSavePrompt`
- `ButtonAction` 枚举：`QuickSave` / `QuickLoad` / `OpenSaveMenu` / `OpenLoadMenu` / `OpenSettings` / `ToggleHide` / `BackToTitle` / `Quit` 等
- `ButtonRect` 结构体：按钮的矩形区域和关联动作
- `SettingsLayout` 结构体：设置页各控件的布局坐标
- `Rect4` 结构体：通用矩形

### 8.2 主要函数

| 函数 | 职责 |
|------|------|
| `run(engine: Engine)` | 主循环入口，管理 UI 状态机 |
| `window_conf()` | 窗口配置（标题、尺寸、图标） |
| `load_font_with_fallback()` | 字体加载与回退 |
| `draw_text_f()` / `measure_text_f()` | 带字体回退的文本绘制/测量 |
| `draw_scene()` | 场景绘制（背景、角色、对话框） |
| `draw_hud_buttons()` | 左下角 HUD 按钮组 |
| `draw_title_screen()` | 标题画面 |
| `draw_settings_menu()` | 设置页（滑块、开关、下拉） |
| `handle_settings_interaction()` | 设置页交互处理 |
| `handle_click()` | 通用点击处理 |
| `handle_button_action()` | 按钮动作执行 |

### 8.3 启动流程

1. `akrs-game/src/main.rs`：加载剧本 → `Engine::new(&script)`（phase = Title）→ `renderer::run(engine)`
2. `run()` 入口：`load_settings()` → 检测 autosave → 设置 `ui_mode`
3. 如果有 autosave 且 `auto_recovery` 为 true：显示 `AutoSavePrompt`
4. 否则进入 `Normal` 模式，phase 为 `Title` 时显示标题画面
5. 玩家点击"开始游戏"→ `engine.start_game()` → phase 变为 `Running`

### 8.4 设置页交互

- **滑块**（文本速度、BGM 音量、音效音量）：鼠标拖拽，`dragging_slider` 跟踪状态
- **开关**（自动恢复、全屏模式）：点击切换
- **下拉菜单**（分辨率）：点击展开列表 → 点击选项选中 → `dropdown_open` 跟踪展开状态

### 8.5 HUD 按钮

左下角 7 个半透明按钮（仅在 `Normal` 模式且未隐藏时显示）：
快存 / 快读 / 存档 / 读档 / 标题 / 设置 / 隐藏

### 8.6 隐藏/显示

点击"隐藏"→ `hud_hidden = true` → 对话框和 HUD 隐藏，仅显示场景。隐藏时点击任意位置恢复显示。

---

## 9. 窗口配置

### 9.1 当前设置

```rust
const WINDOW_WIDTH: i32 = 1920;
const WINDOW_HEIGHT: i32 = 1040;  // 预留 40px 给 Windows 任务栏
```

### 9.2 窗口图标

使用 `kokona.png` 转换的 raw RGBA 数据（`assets/icon_kokona_{16,32,64}.bin`），通过 `include_bytes!` 嵌入。加载失败回退到程序化生成的月牙图标。

图标 bin 文件生成方式：
```bash
python3 -c "
from PIL import Image
img = Image.open('kokona.png').convert('RGBA')
for size in [16, 32, 64]:
    scaled = img.resize((size, size), Image.LANCZOS)
    scaled.tobytes()  # 保存为 .bin
"
```

### 9.3 标题

窗口标题为 `Akizuki*Rustgal`（保持英文，不翻译）。

---

## 10. 汉化策略

### 10.1 翻译范围

| 翻译 | 不翻译 |
|------|--------|
| 标题画面按钮（开始游戏、读取存档、设置、退出） | 窗口标题（Akizuki\*Rustgal） |
| 设置页标签（文本速度、BGM 音量等） | 命令行日志（`[akrs-game] Loaded script: ...`） |
| 存档/读档界面提示 | panic 错误信息 |
| HUD 按钮文字 | 技术性变量名/函数名 |
| 自动恢复弹窗 | 代码注释 |
| 故事结束提示 | |

### 10.2 实现方式

所有界面文本直接硬编码在 `renderer.rs` 中，使用 `draw_text_f()` 渲染。无国际化框架（如 fluent/gettext），因为目标用户为中文用户。

---

## 11. 存档系统

### 11.1 存档结构

- 存档目录：`saves/`
- 存档文件：`save_000.json` ~ `save_009.json`（10 个槽位）
- 自动存档：`saves/autosave.json`
- 设置文件：`saves/settings.json`

### 11.2 Settings 结构

```rust
pub struct Settings {
    pub text_speed: f32,        // 0-999，0 或 999 为瞬间显示
    pub bgm_volume: f32,        // 0.0-1.0
    pub sfx_volume: f32,        // 0.0-1.0
    pub fullscreen: bool,
    pub resolution: (u32, u32), // (width, height)
    pub auto_recovery: bool,    // 是否启用崩溃恢复
}
```

设置在启动时通过 `engine.load_settings()` 加载，在退出/关闭窗口时通过 `engine.save_settings()` 保存。

---

## 12. Windows 发布包

### 12.1 发布包结构

```
akrs-windows/
├── akrs-game.exe           # 图形启动器（~12MB，含嵌入字体）
├── akrs.exe                # 命令行工具（~700KB）
├── lewton.dll              # 音频解码库（~1MB）
├── 启动游戏.bat             # 启动脚本（cd /d "%~dp0" + start akrs-game.exe）
├── README.txt              # 用户使用说明
├── scripts/
│   └── demo.akrs           # 示例剧本
└── assets/
    ├── fonts/
    │   └── NotoSansSC-Regular-subset.ttf
    ├── bg/                 # 背景图片目录
    ├── characters/         # 角色立绘目录
    ├── music/              # 背景音乐目录
    ├── sound/              # 音效目录
    └── title/              # 标题画面素材目录
```

### 12.2 打包命令

```bash
# 编译 Windows 版本
source /root/.cargo/env
cd /workspace/akizuki-rustgal
cargo build --workspace --release --target x86_64-pc-windows-gnu

# 复制到发布包目录
cp target/x86_64-pc-windows-gnu/release/akrs.exe /workspace/akrs-windows/
cp target/x86_64-pc-windows-gnu/release/akrs-game.exe /workspace/akrs-windows/
find target/x86_64-pc-windows-gnu/release/deps -name "lewton.dll" -exec cp {} /workspace/akrs-windows/lewton.dll \;
rm -f /workspace/akrs-windows/lewton-*.dll  # 删除带 hash 的重复 DLL

# 打 zip 包
cd /workspace
zip -r "akrs-windows-发布版.zip" akrs-windows/ -x "*.DS_Store"
```

### 12.3 启动脚本

`启动游戏.bat`（ANSI 编码，无 BOM）：
```bat
cd /d "%~dp0"
start akrs-game.exe
```

### 12.4 panic hook

`akrs-game/src/main.rs` 中设置了 panic hook，崩溃时控制台窗口保持打开，等待用户按 Enter：
```rust
std::panic::set_hook(Box::new(|panic_info| {
    println!("{}", panic_info);
    println!("Press Enter to exit...");
    let _ = std::io::stdin().read_line(&mut String::new());
}));
```

---

## 13. 测试

### 13.1 测试概况

- **64 个测试**，全部通过，6 个忽略
- 全部为内联 `#[test]` 单元测试，无独立 `tests/` 目录
- `akrs-render` 和 `akrs-game` 无测试

### 13.2 测试分布

| Crate | 文件 | 测试数 |
|-------|------|--------|
| akrs-core | lexer.rs | 10 |
| akrs-core | parser.rs | 10 |
| akrs-core | vm.rs | 6 |
| akrs-runtime | engine.rs | 7 |
| akrs-runtime | save_load.rs | 4 |
| akrs-runtime | game_state.rs | 5 |
| akrs-runtime | transition.rs | 3 |
| akrs-runtime | hot_reload.rs | 1 |
| akrs-cli | migrate.rs | 6 |
| akrs-pack | lib.rs | 8 |
| akrs-editor | lib.rs | 4 |

---

## 14. 已知警告

Rust 1.92.0 + edition 2024 下 `cargo build --workspace` 产生 0 个错误。本项目自身代码警告 17 个，macroquad 上游补丁警告 28 个（因新版 Rust 默认 lint 更严格，属上游代码问题，非本项目责任）。按 crate 分布：

### akrs-render（10 个，最集中）
- unused imports: `SaveMetadata`, `TransitionOverlay`
- unused variable: `prev_music`（3 处）, `total_h`
- dead code: `label` 字段, `BackToGame` 枚举变体, `cycle_resolution` 函数
- unused doc comment

### akrs-core（5 个）
- unused variable: `file_id`, `source`
- unused mut: `branches`
- dead code: `source` 字段, `section_map` 字段

### akrs-runtime（2 个）
- dead code: `ease_in`, `ease_out` 函数

### akrs-cli（0 个，已修复）
- 原 `mismatched_lifetime_syntaxes` 警告（Rust 1.92 新 lint）已在 `migrate.rs` 修复：`Vec<IndentedLine>` → `Vec<IndentedLine<'_>>`

### macroquad 补丁（28 个，来自上游）
- edition 2018 上游代码，新版 Rust 默认 lint 更严格导致。仅 `src/text.rs` 一处为本项目修改（CJK 字体补丁），其余警告属上游 macroquad 0.3.26 代码。

这些警告不影响功能，建议清理或加 `#[allow(dead_code)]`。

---

## 15. 历史踩坑记录

以下问题在开发过程中遇到过并已修复，记录在此避免重复踩坑：

### 15.1 macroquad "Vertical fonts are not supported" panic
- **原因**：CJK 字体的 vertical metrics 导致 fontdue 返回非零 `advance_height`
- **修复**：本地补丁删除 panic 检查（见第 5 节）

### 15.2 macroquad "no entry found for key" panic
- **原因**：错误地将 panic 改为 `return;`，导致字符未缓存，后续读取时 panic
- **修复**：删除整个 if 块，让缓存逻辑继续执行

### 15.3 字体加载失败导致崩溃
- **修复**：改用 `load_ttf_font_from_bytes`（同步）替代 `load_ttf_font`（async），配合 `include_bytes!` 嵌入

### 15.4 启动跳过标题界面
- **原因**：`akrs-game` 使用了 `Engine::start_running()`（直接进入游戏）
- **修复**：改为 `Engine::new()`（phase = Title，显示标题画面）

### 15.5 .bat 文件编码问题
- **原因**：UTF-8 BOM 在 Windows CMD 中导致乱码
- **修复**：使用 ANSI 编码，无 BOM，内容仅两行 `cd /d "%~dp0"` + `start akrs-game.exe`

### 15.6 窗口高度超出任务栏
- **原因**：窗口高度 1080px 在有任务栏的屏幕上底部被遮挡
- **修复**：默认窗口高度改为 1040px

### 15.7 "音"字等部分字符无法显示
- **原因**：精简字体可能遗漏部分字符
- **修复**：实现字符级回退（主字体 → 系统字体 → □ 占位符）

### 15.8 edition 2024 迁移（Rust 1.75 → 1.92）
- **变更**：工具链从 1.75.0 升级到 1.92.0，edition 从 2021 升级到 2024，新增 `rust-toolchain.toml` 锁定版本
- **破坏点 1**：edition 2024 中 `match` 隐式借用模式禁止显式 `ref` 绑定。`lexer.rs:479` 的 `TokenKind::String(ref s)` 改为 `TokenKind::String(s)`
- **破坏点 2**：Rust 1.92 新增 `mismatched_lifetime_syntaxes` lint。`migrate.rs:28` 的 `Vec<IndentedLine>` 改为 `Vec<IndentedLine<'_>>`
- **保留**：`eframe` 的 `default-features = false` 保留为设计选择（不需要 accesskit），但注释更新——Rust 1.75 的 atspi/zbus 不兼容问题已不再适用
- **未升级**：`eframe 0.21`、`egui 0.21`、`macroquad 0.3.26` 等依赖主版本未升级（仍可在 1.92 编译），升级主版本属独立大任务

---

## 16. 待办事项与改进建议

### 16.1 高优先级
- [ ] 初始化 Git 仓库，添加 `.gitignore`（至少排除 `target/`、`saves/`）
- [ ] 编写根目录 `README.md`
- [ ] 清理 17 个本项目编译警告（尤其是 `akrs-render` 的 10 个）
- [ ] 为 `akrs-render` 和 `akrs-game` 添加测试

### 16.2 中优先级
- [ ] 添加 CI 配置（GitHub Actions：lint + test + build）
- [x] 升级 Rust 工具链至 1.92.0 + edition 2024（已完成）
- [ ] 升级依赖主版本（eframe/egui 0.21 → 最新、macroquad 0.3 → 0.4），属独立大任务，需重写编辑器/渲染器 API 调用
- [ ] 字体精简：进一步缩减 `NotoSansSC-Regular-subset.ttf` 体积
- [ ] 实现实际的全屏模式切换（当前设置页有开关但可能未完全实现）
- [ ] 添加背景图片/角色立绘/音乐的实际加载逻辑（当前 AssetManager 可能仅有骨架）

### 16.3 低优先级
- [ ] 支持更多过渡效果
- [ ] 添加更多剧本示例
- [ ] 编辑器功能完善
- [ ] 国际化框架（如果需要支持多语言）

---

## 17. 关键文件速查

| 文件 | 行数 | 说明 |
|------|------|------|
| `crates/akrs-render/src/renderer.rs` | ~1780 | 渲染器主体，UI 逻辑，字体系统 |
| `crates/akrs-runtime/src/engine.rs` | ~1010 | 引擎核心，游戏状态机 |
| `crates/akrs-core/src/parser.rs` | ~801 | DSL 解析器 |
| `crates/akrs-cli/src/migrate.rs` | ~995 | Ren'Py → .akrs 迁移工具 |
| `crates/akrs-editor/src/lib.rs` | ~766 | egui 编辑器 |
| `crates/akrs-core/src/vm.rs` | ~535 | DSL 虚拟机 |
| `crates/akrs-core/src/lexer.rs` | ~515 | DSL 词法分析器 |
| `crates/akrs-runtime/src/save_load.rs` | ~449 | 存档系统 |
| `crates/akrs-runtime/src/settings.rs` | ~120 | 设置结构体与持久化 |
| `crates/akrs-game/src/main.rs` | ~60 | 图形启动器入口 |
| `patches/macroquad/src/text.rs` | ~340 | macroquad 字体补丁（仅改 1 处） |
| `scripts/demo.akrs` | 108 | 示例剧本 |
| `docs/dsl-specification.html` | 1299 | DSL 规范文档 |

---

## 18. 联系信息

- 项目名称：Akizuki\*Rustgal
- 引擎描述：A visual novel engine built in Rust with compile-time DSL checking
- 许可证：MIT
- 当前版本：1.0.0
