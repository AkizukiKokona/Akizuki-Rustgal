# 升级待办清单

> 隐藏持久化记录，防止会话上下文丢失后找不到进度。非用户文档。

## 已完成

- 任务3：渲染后端 macroquad → wgpu 22 + winit 0.29
- 任务4：游戏内 UI 圆角化（wgpu 新增圆角矩形/线条图元 API + renderer 11 类元素圆角）
- 任务5：编辑器 eframe/egui → iced 0.13（含 14 项高级 UI 全部接线）

## 待办（用户已确认全部执行，按推荐方案）

- [x] **音频**：重写 audio.rs 封装层（86d356b）。SoundKind 三轨路由 + BGM 流式 + crossfade，settings 三轨音量生效。
- [x] **文本**：text.rs 改用 cosmic-text Buffer/Layout（86d356b）。shaping/bidi/kerning/字符级回退，签名全兼容。
- [x] **游戏内 UI**：自写组件化改进完成。新增 ui_widgets.rs（draw_slider/draw_dropdown/draw_text_input 三个可复用组件），4 个下拉 bool 合并为 open_dropdown:Option<DropdownId>，删除 8 个冗余助手函数；补 IME 输入法（wgpu_backend 接 winit Ime 事件，文本输入聚焦时 set_ime_allowed + preedit 下划线显示 + commit 插入）。保留立即模式架构。
  - 注：原方案"嵌入 iced 作运行时 UI"经调研确认不可行——iced_wgpu 0.13.5 锁 wgpu 0.19，与 akrs-render 的 wgpu 22 类型不兼容无法共享 device。用户决策改走自写组件化。
- [x] **编辑器 UI**：引入 iced_aw 0.10 扩展（74f6eb7）。Card 统一 11 个弹窗卡片 + Tabs 重构右栏预览标签页。

## 工具链约束（勿破）

- Rust 1.92.0，wgpu 22，winit 0.29，iced 0.13
- **勿升 iced 0.14**（用 wgpu 27，与 akrs-render 的 wgpu 22 全项目冲突）
- akrs-editor 已在 workspace members 中，eframe/egui 已移除

## 临时文件（不上传）

- `.task4_notes.tmp` / `.task5_notes.tmp`：会话临时记录，git 忽略
