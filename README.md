<div align="center">

# Akizuki\*Rustgal

**纯 Rust 打造的视觉小说引擎 — 编译时检查、自定义 DSL、跨平台运行**

[![License](https://img.shields.io/badge/license-MIT-blue.svg)](#许可证)
[![Rust](https://img.shields.io/badge/rust-1.92+-orange.svg)](https://www.rust-lang.org)
[![Version](https://img.shields.io/badge/version-1.0.93-success.svg)](#)
[![Platform](https://img.shields.io/badge/platform-Windows%20%7C%20Linux%20%7C%20macOS-lightgrey.svg)](#下载与安装)
[![PRs Not Recommended](https://img.shields.io/badge/PRs-not%20recommended-yellow.svg)](#贡献)

</div>

---

> **关于代码**：本项目代码实现由 AI 负责，但我会对里面的每一行代码负责。

## 独有特色

> **语言 / Language:** 简体中文 | [English](#unique-highlights) | [日本語](#独自の特徴) | [繁體中文](#獨有特色)

对比 Ren'Py（Python 运行时）、KiriKiri/KAG（TJS 解释器）等主流视觉小说引擎，Akizuki\*Rustgal 凭借纯 Rust + 自定义 DSL 实现了以下独有能力：

- **编译时剧本纠错** — 未定义跳转、重复节名、缺失资源、舞台角色超限在 `cargo build` 阶段即报错，无需运行即可发现（Ren'Py / KiriKiri 均为运行时检查）
- **内置可视化编辑器** — egui 驱动的 GUI 编辑器：语法高亮 + 立绘/背景/音乐实时预览 + 便携查找替换插入，告别纯文本编辑（Ren'Py 仅 Launcher 文本编辑，KiriKiri 依赖外部编辑器）
- **对照翻译模式** — 编辑器内原文-译文并排，原文只读、译文实时写入；翻译文件以原文为 key，仅改动的行需重译（Ren'Py 用哈希 ID 翻译块，与脚本结构耦合）
- **过程宏编译期嵌入** — `.akrs` 脚本通过过程宏在编译时嵌入 Rust 代码，类型检查延伸至剧本层（其他引擎均为运行时解析）
- **满血 Rust 零运行时** — 无 Python、无 TJS、无 GC 停顿，单二进制分发，帧率稳定可预测

<details>
<summary><b>English</b></summary>

### Unique Highlights

Compared to Ren'Py (Python runtime) and KiriKiri/KAG (TJS interpreter), Akizuki\*Rustgal leverages pure Rust plus a custom DSL to deliver capabilities no other VN engine offers:

- **Compile-time script checking** — Undefined jumps, duplicate section names, missing assets, and stage character overflow are caught at `cargo build` time, before the game ever runs (Ren'Py and KiriKiri only detect these at runtime)
- **Built-in visual editor** — An egui-powered GUI editor with syntax highlighting, live sprite/background/music preview, and find-and-replace insertion — no more plain-text editing (Ren'Py ships only a launcher text editor; KiriKiri relies on external editors)
- **Side-by-side translation mode** — The editor shows original and translation columns; the original is read-only and the translation writes back live. Translation files key on the original text, so only edited lines need retranslation (Ren'Py uses hash-ID translate blocks coupled to script structure)
- **Compile-time script embedding via proc macros** — `.akrs` scripts are embedded into Rust code at compile time via procedural macros, extending type checking into the script layer (other engines parse scripts at runtime)
- **Full Rust, zero runtime** — No Python, no TJS, no GC pauses; a single binary ships with stable, predictable frame rates

</details>

<details>
<summary><b>日本語</b></summary>

### 独自の特徴

Ren'Py（Python ランタイム）や KiriKiri/KAG（TJS インタプリタ）などの主要ノベルエンジンと比較し、Akizuki\*Rustgal は純 Rust + カスタム DSL により他にない機能を実現しています：

- **コンパイル時スクリプト検査** — 未定義ジャンプ、重複セクション名、欠落アセット、ステージキャラ超過は `cargo build` 段階で検出され、実行せずに問題を発見（Ren'Py / KiriKiri は実行時検査のみ）
- **内蔵ビジュアルエディタ** — egui 駆動の GUI エディタ。シンタックスハイライト + 立絵/背景/音楽のライブプレビュー + 検索置換挿入。プレーンテキスト編集から解放（Ren'Py はランチャーのテキストエディタのみ、KiriKiri は外部エディタ依存）
- **対照翻訳モード** — エディタ内で原文-訳文を並列表示。原文は読み取り専用、訳文はリアルタイム反映。翻訳ファイルは原文をキーとするため、変更した行のみ再翻訳（Ren'Py はハッシュ ID 翻訳ブロックでスクリプト構造と結合）
- **プロシージャルマクロによるコンパイル時埋め込み** — `.akrs` スクリプトはプロシージャルマクロでコンパイル時に Rust コードへ埋め込まれ、型検査がスクリプト層まで及ぶ（他エンジンは実行時解析）
- **完全 Rust・ランタイムゼロ** — Python なし、TJS なし、GC 停止なし。シングルバイナリで配布、安定した予測可能なフレームレート

</details>

<details>
<summary><b>繁體中文</b></summary>

### 獨有特色

相較於 Ren'Py（Python 執行時期）與 KiriKiri/KAG（TJS 直譯器）等主流視覺小說引擎，Akizuki\*Rustgal 藉由純 Rust + 自訂 DSL 實現了以下獨有能力：

- **編譯時劇本檢查** — 未定義跳轉、重複章節名、缺失資源、舞台角色超限在 `cargo build` 階段即報錯，無需執行即可發現（Ren'Py / KiriKiri 皆為執行時期檢查）
- **內建視覺化編輯器** — egui 驅動的 GUI 編輯器：語法高亮 + 立繪/背景/音樂即時預覽 + 查找取代插入，告別純文字編輯（Ren'Py 僅 Launcher 文字編輯器，KiriKiri 依賴外部編輯器）
- **對照翻譯模式** — 編輯器內原文-譯文並排，原文唯讀、譯文即時寫入；翻譯檔案以原文為鍵，僅改動的行需重譯（Ren'Py 使用雜湊 ID 翻譯區塊，與腳本結構耦合）
- **過程巨集編譯期嵌入** — `.akrs` 腳本透過過程巨集在編譯時嵌入 Rust 程式碼，型別檢查延伸至劇本層（其他引擎皆為執行時期解析）
- **滿血 Rust 零執行時期** — 無 Python、無 TJS、無 GC 停頓，單一二進位散布，幀率穩定可預測

</details>

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
| **跨平台编译** | 一套代码编译到 Windows / Linux / macOS 三大桌面平台 |

## 功能特性

- **自定义剧本语言** — 专为视觉小说设计的 `.akrs` DSL，支持对话、旁白、背景切换、立绘上场/下场、分支选择、变量、条件跳转
- **编译时检查** — 在运行之前就发现错误：未定义的章节跳转、重复的节名、缺失的资源引用、舞台角色超限
- **12 种过渡效果** — `fade`、`dissolve`、`slide_left/right/up/down`、`wipe_left/right`、`blur`、`fade_black`、`fade_white`、`instant`
- **UI 页面切换动画** — 标题 / 设置 / 存档 / 读档等页面切换均带 0.5 秒淡入淡出过渡（淡出 + 淡入合计 0.5 秒）；设置内标签页切换为即时切换不加过渡；确认对话框、备注编辑等模态弹窗带 0.2 秒内容淡入，避免瞬间弹出
- **多分辨率自适应** — 所有立绘位置使用百分比坐标（0.0–1.0），在 1080p / 1440p / 4K 下布局比例一致
- **高 DPI 适配** — 启用 `high_dpi` 渲染模式，UI 缩放按逻辑像素计算，在 125% / 150% / 200% 等 DPI 倍率下控件不再溢出堆叠，高 DPI 屏幕文字自动更清晰
- **可视化编辑器** — 基于 egui 的剧本编辑器，支持语法高亮、立绘预览、实时调参
- **存档系统** — 多槽位存档/读档，支持崩溃恢复；快速存档使用独立不可见槽位，不覆盖手动存档；读档/崩溃恢复时完整还原背景、立绘、音乐等场景状态（含过渡中途存档：存档时把待应用的过渡变更一并写入快照，读档后清空残留过渡状态，避免立绘"下不去"）；存档页每个槽位显示 16:9 长方形场景缩略图（与游戏画面比例一致，按场景快照重绘背景与立绘，contain 模式完整显示不裁切），空槽位或旧存档显示「无预览」占位文字；支持玩家自定义备注（Enter 确认 / Esc 取消 / Backspace 删除）
- **热重载** — 修改剧本后自动重新编译，无需重启游戏
- **快进模式** — 支持仅文本快进和包含语音快进两种模式
- **设置系统** — 文字速度、音量、分辨率、全屏等可配置项
- **多语言翻译系统** — 原文剧本不修改，翻译文件与原文一一对应；支持章节标题、对话、旁白、分支选项、角色名翻译；编辑器内置对照翻译模式（原文-译文并排，原文只读）；CLI 一键生成翻译骨架；运行时设置页实时切换语言（剧本语言与 UI 语言独立切换，放弃未应用设置时正确回退）；UI 文本翻译内置英文兜底，语言文件缺失或部分 key 未译时回退英文而非显示 raw key；返回标题/故事结束后重建引擎时自动继承翻译目录，语言切换能力不丢失
- **编辑器内置打包** — GUI 勾选 Windows/Linux/macOS 平台，一键 `cargo build --release --target`，自动收集产物到 `build/` 目录
- **编辑器游戏预览** — 点击按钮即可在独立窗口启动游戏预览，编辑器可继续编辑，互不阻塞
- **开屏页（标题页）可配置** — 通过 `project.json` 的 `title_background` 与 `title_music` 字段自定义标题页背景图与背景音乐（编辑器「项目设置」内可编辑）；留空时背景回退到 `assets/title.png`、音乐回退到 `assets/music/title_bgm.mp3`，二者皆无则静音。**本仓库自带的 demo 不含任何音频资源，也未配置开屏页音乐**，因此默认标题页静音——但引擎完整支持自定义开屏页音画。
- **主题配色可自定义** — 通过 `project.json` 的 `theme` 字段（编辑器「项目设置 → 主题配色」内用调色板或十六进制输入框编辑）自定义游戏内 3 种项目级颜色：主题色1（模态对话框/面板背景）、主题色2（按钮背景，悬停/按下态由引擎自动派生）、对话框色（游戏进行中文本框渐变基色）。各字段留空或缺失时回退引擎内置配色，旧项目文件无需改动即可加载。文字颜色不在编辑器配置——由游戏运行时按「已读 / 未读」自动着色，玩家可在游戏内设置页自定义（详见下条）。
- **已读 / 未读文字着色** — 引擎记录每句对话/旁白是否被玩家看过（`saves/read_history.json`，以原文 `speaker|text` 为 key，跨语言稳定）。首次出现的句子视为「未读」，默认白色；再次出现视为「已读」，默认浅紫色。角色名与对白正文统一按已读/未读着色。同时修复了「仅跳过已读」快进此前用翻译后文本拼 key 导致永远判定为未读、快进卡死的缺陷——现直接复用对话的 `is_read` 标志。开发者模式标签页提供「将全部文本设为未读」一键清空已读历史。
- **配色自定义标签页** — 游戏内设置页新增「配色」标签页，玩家可用十六进制输入框（支持 `#RRGGBB` / `#RRGGBBAA`，实时预览）或 12 色预设调色板自定义 5 种颜色：已读文字色、未读文字色，以及 3 种主题色（面板背景 / 按钮背景 / 对话框渐变）。主题色带「项目默认 / 自定义」开关——不勾选即沿用 `project.json` 的项目主题，勾选后可覆盖为玩家自选色。改色即时生效（每帧重算有效主题），应用设置后持久化到 `saves/settings.json`。
- **开发者模式标签页** — 设置页新增「开发者」标签页（位于「配色」与「帮助」之间），集中放置高危调试项。标题下方以红字警告「请不要随便动本页的内容」。原「画面」标签页的「显示终端调试输出」开关移入此页；另提供「将全部文本设为未读」按钮，一键清空 `saves/read_history.json`，使所有句子重新被视为未读（已读/未读着色随之重置）。该页改动同样纳入「有未应用更改」检测。
- **章节切换动画** — `# 章节名 显示标题` 语法将章节名与显示标题分开（标题可含空格）。通过 `->` 跳转到带标题的章节时，游戏先播放 0.5 秒全屏淡入淡出（覆盖文本框等全部内容），随后从屏幕顶部滑入一条白底通知（约 30% 透明），章节名与标题分两行居中显示，停留 1 秒后按原路径滑出。文本过长时通知自动缩小字号以适配屏宽。编辑器在标题超过 24 字符时给出非阻断警告。`=>` 访问子章节不触发该通知。
- **背景交叉淡入** — 同一章节内 `@bg X with fade`（或 `dissolve`）切换背景时，背景层做交叉淡入（旧背景淡出、新背景淡入，0.5 秒），不画全屏黑场，对话框等 UI 在背景之上正常显示、不被遮挡。角色上下场的 `with fade` 仍走全屏遮罩过渡，不受影响。
- **脚本驱动的文本框隐藏（`- hide`）** — 在剧本中写 `- hide` 可暂时隐藏文本框与 HUD（效果等同手动点 UI 的「隐藏」），直到下一条对话/旁白/选项出现时自动恢复。用于在一段纯场景变换（换背景、立绘上下场）期间让画面保持干净。编辑器翻译功能自动忽略该语句。
- **编辑器 `=>`/`<=` 配对辅助** — 仅影响编辑器视觉呈现，不修改脚本语法或编译逻辑。当光标停留在 `=>`（访问子章节）或 `<=`（返回）指令上时，高亮其按文本顺序最近未配对的对应指令（`=>` 找下一个 `<=`，`<=` 找上一个 `=>`，栈式匹配，不考虑嵌套语义）。鼠标悬停在 `=>` 上显示「跳转到目标：<目标章节名>」与「返回点在第 X 行」；悬停在 `<=` 上显示「返回到上一个拜访点」。右栏新增「大纲」标签页，按缩进展示章节（`#`）、分支选项（`?`/`|`）与 `=>`/`<=` 配对结构——每个 `=>` 与其匹配 `<=` 之间的内容缩进一级，形成「拜访块」层级。
- **剧本错误冗余降级** — 剧本编译/加载失败时游戏不崩溃，自动降级为「仅标题页」模式：玩家仍可进入标题页、正常修改设置，但点击「开始游戏 / 读档 / 继续游戏」时会弹出剧本错误警告对话框（支持四国语言：简中 / 英文 / 日文 / 繁中）。自动恢复在此模式下失效。错误详情同时输出到控制台。引擎的设置、存档管理等非剧本功能不受影响，保证剧本问题不波及引擎其他部分。
- **隐藏结局（彩蛋尾声）** — 剧本可声明隐藏结局作为彩蛋：`ending "id" epilogue "path.akrs" [button "文本"]` 声明尾声剧本与主页按钮文本（省略 `button` 时用翻译键 `title.epilogue`，默认「尾声之后」/「After the Ending」/「エピローグの後に」/「尾聲之後」，四语言齐全）；剧本中 `unlock "id"` 标记达成条件后解锁（持久化到 `saves/endings.json`，跨周目保留）。达成结局后自动淡入淡出返回主页（无需用户确认），主页出现「尾声之后」按钮，点击即加载对应尾声剧本播放，播放结束后再次淡入淡出回主页。支持多个隐藏结局，每个结局独立声明按钮文本与尾声路径。编辑器工具栏提供 `ending`/`unlock` 语法快速插入按钮与高亮。
- **快读按钮禁用态** — 无快存时 HUD「快读」按钮变为灰色低透明态且点击无操作（不再误触回标题），避免玩家在无快存情况下点快读导致意外退出游戏。
- **项目警告提示** — 项目可在 `project.json` 的 `warning` 字段（编辑器「项目设置」内可编辑）留一段作者提示文字（彩蛋/版权声明等）。用编辑器打开本项目时自动弹出小窗显示该文字，玩家可点「确定」仅本次关闭，或点「不再显示」永久忽略（仅本地生效，存于编辑器数据目录 `dismissed_warnings.json`，按项目规范化路径记录，不随项目分发）。本仓库 demo 即带一条版权声明作为示例。
- **弹窗防截断** — 编辑器中内容较多的弹出窗口（项目设置 / 快捷键帮助 / Cargo 安装引导 / 项目警告）内容统一包入竖向滚动区，窗口高度受限不超过主窗口，内容超出时在窗内滚动而非被截断；项目设置窗口改为可缩放。彻底解决小窗口下表单底部按钮被顶出可视区的问题。
- **预览精确运行当前剧本** — 编辑器点「预览游戏」时把当前编辑的文件路径以 `--script` 参数显式传给游戏，确保预览运行的就是用户当前编辑的剧本，而非 `project.json` 的 `main_script` 或内置 demo 回退（修复了「保存后预览却打开内置 demo」的问题）。游戏入口 `akrs-game` 新增 `--script`/`-s` 参数，剧本加载优先级为 `--script` 显式参数 > `project.json` main_script > `scripts/demo.akrs` 回退。未保存到磁盘的新建文件点预览时弹「请先保存再预览」提示；保存时若文件名与 main_script 不一致，状态栏追加非阻断提示。
- **蓝图模式（可视化节点编辑）** — 工具栏「蓝图模式」按钮切换到类 Unreal Engine 蓝图的流程图编辑界面（基于 egui 自绘）。剧本的每一行变成画布上的一个节点（17 种类型，各自带颜色标识），节点之间用贝塞尔曲线连线表示执行顺序。节点正文显示友好文本（自动去掉脚本前缀符号），超长文本自动换行并裁剪。支持左键拖动节点移动、左键拖动输出引脚到输入引脚创建连线、右键拖动空白平移画布、Ctrl+滚轮光标锚点缩放、右键空白弹出 14 种节点模板菜单、右键节点弹出操作菜单、双击节点就地编辑、Delete 删除选中节点。自动布局按章节分列、一列纵向不限、列内水平居中对齐（连线保持垂直直线）。选择块拖出时自动生成 Choice + 2 个 Option 节点 + 连线，结尾 `?` 自动补全。蓝图模式下右侧预览面板显示积木卡片预览，按钮自动适配为「添加为节点 / 替换选中节点」。
- **Ctrl+点击跳转预览** — 在剧本编辑器中按住 Ctrl 点击 `@bg 背景` / `@music 音乐` / `+ 角色` 等行（或按 Ctrl+J），右侧预览面板自动跳转到对应标签页并选中该资源，无需手动切换标签页翻找。egui 的 TextEdit 不暴露带修饰键的点击事件，编辑器通过全局事件过滤器追踪 Ctrl 按下/释放状态实现该功能。

## 快速开始

### 下载与安装

前往 [Releases](https://github.com/AkizukiKokona/Akizuki-Rustgal/releases) 下载最新版本：

- **Windows**：下载 `.zip`，解压后双击 `启动游戏.bat`
- **Linux / macOS**：下载 `.tar.gz`，解压后运行 `./启动游戏.sh`

### 从源码构建

```bash
# 克隆仓库
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
>
> **跨平台说明**：编辑器与运行时均会按平台自动选择系统中文字体（Windows 微软雅黑 / macOS 苹方 / Linux Noto 或文泉驿）、数据目录（Windows `%APPDATA%` / macOS `~/Library/Application Support` / Linux `~/.local/share`）与二进制名（Windows `akrs.exe` / 其他 `akrs`），无需手动配置。

## 剧本语言速览

```ruby
# 夏日祭的尾声

// 背景：夏日祭夜晚
@bg bg1 with fade

// 立绘：心夏（身体1）居中
+ 心夏 (kokonabody1) 居中

心夏: "引航者，今天晚上玩得开心吗？"

// 差分更换：仅换立绘图片，位置/大小不变，无过渡
+ 心夏 (kokonabody2) swap

引航者: "不错不错，可惜今晚是夏日祭最后一天了。"

// 旁白
"心夏低下了头，不过不到一秒她又把头抬起来了。"

// 分支选项
? "引航者要对心夏说什么？"
| "我有个小礼物想送给心夏" -> 分支A
| "我给心夏买了一套衣服" -> 分支B
?

// 变量与条件
$好感度 = 10
if 好感度 >= 10 then
  心夏: "嘿嘿，也许我有呢？"
end

// 切换背景 + CG
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
akrs-render    渲染层：基于 wgpu/winit 的 2D 渲染器
akrs-editor    可视化编辑器：基于 egui 的剧本编辑与预览工具
akrs-pack      打包工具：将游戏资源打包为可分发包
akrs-cli       命令行工具：检查、运行、打包
akrs-game      游戏启动器：读取剧本并启动图形界面
```

## 技术栈

本项目完全使用 Rust 编写，依赖以下优秀的 Rust 生态项目：

| 依赖 | 用途 |
|------|------|
| [**wgpu**](https://github.com/gfx-rs/wgpu) | 跨平台图形 API 抽象层，提供 Vulkan/DX12/Metal/GL 后端渲染 |
| [**winit**](https://github.com/rust-windowing/winit) | 跨平台窗口管理与事件系统 |
| [**eframe/egui**](https://github.com/emilk/egui) | 跨平台 GUI 框架，用于编辑器界面 |
| [**serde**](https://github.com/serde-rs/serde) / serde_json | 序列化框架，用于存档和设置持久化 |
| [**syn**](https://github.com/dtolnay/syn) / quote / proc-macro2 | 过程宏工具链，用于编译时脚本嵌入 |
| [**notify**](https://github.com/notify-rs/notify) | 文件系统监听，用于热重载 |
| [**codespan-reporting**](https://github.com/brendanzab/codespan) | 编译错误诊断信息格式化 |
| [**cosmic-text**](https://github.com/pop-os/cosmic-text) | 文本排版引擎，提供文字 shaping/layout/raster |
| [**kira**](https://github.com/tesselode/kira) | 音频引擎，提供播放控制与混音 |

## 文档

| 文档 | 说明 |
|------|------|
| [剧本语言规范](docs/剧本语言规范.md) | `.akrs` 剧本语言的完整语法参考，涵盖所有指令、变量、流程控制和编译时检查规则 |
| [立绘摆放语法](docs/立绘摆放语法.md) | 立绘位置、大小、过渡效果的详细说明与编辑器预览指南 |
| [翻译功能使用说明](docs/翻译功能使用说明.md)（[EN](docs/translation-guide.en.md) / [JA](docs/translation-guide.ja.md) / [ZH-TW](docs/translation-guide.zh-TW.md)） | 多语言翻译系统完整指南：翻译文件格式、CLI 骨架生成、编辑器对照翻译模式、运行时语言切换 |
| [快捷键说明](docs/快捷键说明.md) | 游戏与编辑器的全部键盘 / 鼠标快捷键对照表，含三平台差异说明 |
| [错误代码说明](docs/错误代码说明.md) | 仿 Windows 蓝屏错误界面的 16 进制错误代码对照表、蓝屏按钮操作与日志导出格式说明 |
| [DSL 规范（交互式 HTML）](docs/dsl-specification.html) | 带语法高亮和侧边导航的网页版语法文档 |

## 编辑器功能

### 资源预览面板

右侧边栏提供五个预览标签页：

- **剧本预览**：运行当前剧本，实时查看对话、选项和场景状态
- **立绘预览**：选择立绘图片，调整位置和大小，生成 `+ 角色` 入场语法
- **背景预览**：选择背景图片，指定过渡效果，生成 `@bg 背景` 语法
- **音乐预览**：选择音乐文件，生成 `@music` 播放语法和 `@stop_music` 关闭语法
- **大纲**：按缩进展示章节（`#`）、分支选项（`?`/`|`）与 `=>`/`<=` 配对结构，每个 `=>` 与其匹配 `<=` 之间的内容缩进一级，形成「拜访块」层级

除大纲外，每个预览面板都支持：
- 复制语法到剪贴板
- 追加语法到脚本末尾
- **替换插入**：弹窗输入要查找的内容，替换为生成的语法（更人性化）
- **放大预览**：打开弹窗显示更大尺寸预览（1920×1080 分辨率）

立绘预览还支持 **隐藏立绘** 按钮，生成 `- 角色` 出场语法。

### 蓝图模式（可视化节点编辑）

点击工具栏「蓝图模式」按钮进入可视化节点编辑界面（灵感来自 Unreal Engine 蓝图，精简版，基于 egui 自绘实现）。剧本的每一行被转换为画布上的一个节点，节点之间用贝塞尔曲线连线表示执行顺序，比逐行点击插入更直观。

**节点类型**（17 种，由行首标记自动推断，各自带颜色标识）：

| 类型 | 行首标记 | 类型 | 行首标记 |
|------|---------|------|---------|
| 章节 | `#` | 跳转 | `->` |
| 对话 | `角色:` | 访问 | `=>` |
| 旁白 | `"..."` | 返回 | `<=` |
| 命令 | `@` | 等待 | `~~` |
| 立绘 | `+` / `-` | 故事结束 | `end` |
| 选择块 | `?` | 结局声明 | `ending` |
| 选择选项 | `\|` | 解锁 | `unlock` |
| 变量 | `$` | 注释 | `//` |
| 其他 | 无法识别的行 | | |

**交互方式**（参考 UE 蓝图，精简）：

| 操作 | 功能 |
|------|------|
| 左键点击节点 | 选中 |
| 左键拖动节点 | 移动节点位置 |
| 左键拖动输出引脚 → 输入引脚 | 创建连线 |
| 右键拖动空白 | 平移画布 |
| Ctrl + 滚轮 | 光标锚点缩放（0.2×–3.0×） |
| 右键点击空白 | 弹出添加节点菜单（13 种模板） |
| 右键点击节点 | 弹出节点操作菜单（删除 / 编辑） |
| 双击节点 | 就地编辑节点文本 |
| Delete | 删除选中节点 |
| Escape | 关闭菜单 |

**工具栏操作**：从脚本导入（解析当前脚本文本生成节点 + 自动布局）、生成脚本（遍历连线图输出脚本文本）、整理布局（按章节分组自动排列）、缩放放大 / 缩小 / 重置视图。

进入蓝图模式时自动从当前脚本导入节点并自动布局，退出时自动导出为脚本文本。

**与预览面板联动**：蓝图模式下右侧预览面板的按钮自动适配——

| 预览面板 | 文本模式按钮 | 蓝图模式按钮 |
|---------|------------|------------|
| 立绘预览 | 追加到脚本 / 隐藏此立绘 / 替换插入 | 添加为节点 / 添加下场节点 / 替换选中节点 |
| 背景预览 | 追加到脚本 / 替换插入 | 添加为节点 / 替换选中节点 |
| 音乐预览 | 追加播放语法 / 追加关闭语法 / 替换插入 | 添加播放节点 / 添加关闭节点 / 替换选中节点 |

工具栏的「插入语法」下拉菜单在蓝图模式下也变为「插入节点」，点击直接添加节点到画布。新节点自动放在最后一个节点下方。

### 翻译模式防呆设计

在「对照翻译」模式下，右侧资源预览边栏自动隐藏，避免误操作增删资源（翻译模式不需要修改剧本）。

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

欢迎通过 Issue 反馈 bug 与功能建议。

关于 Pull Request：**不建议**提交 PR。本项目坚持 **100% 纯 Rust** 实现——从词法分析到渲染、编辑器，每一行代码都是 Rust，不依赖任何脚本运行时（无 Python、无 Lua、无 TJS）。这一原则是项目的核心定位，因此外部 PR 通常难以合入：一方面维护者需要确保每一处改动都保持纯 Rust，审查成本较高；另一方面若改动引入了非 Rust 组件或通过 FFI 调用非 Rust 库，会破坏项目的设计目标。如果您发现了 bug 或有功能需求，**更推荐通过 Issue 描述**，维护者会评估并在纯 Rust 前提下实现。当然，如果您确实有纯 Rust 的改进且愿意遵守项目规范，也可以提交 PR，但请在提之前先在 Issue 中讨论。

1. Fork 本仓库
2. 创建功能分支（`git checkout -b feature/amazing-feature`）
3. 提交更改（`git commit -m 'Add amazing feature'`）
4. 推送到分支（`git push origin feature/amazing-feature`）
5. 创建 Pull Request

## 许可证

本项目基于 [MIT 许可证](LICENSE) 开源。
