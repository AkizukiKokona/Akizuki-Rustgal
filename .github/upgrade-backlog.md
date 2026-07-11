# 升级待办清单

> 隐藏持久化记录，防止会话上下文丢失后找不到进度。非用户文档。

## 已完成

- 任务3：渲染后端 macroquad → wgpu 22 + winit 0.29
- 任务4：游戏内 UI 圆角化（wgpu 新增圆角矩形/线条图元 API + renderer 11 类元素圆角）
- 任务5：编辑器 eframe/egui → iced 0.13（含 14 项高级 UI 全部接线）

## 待办（用户已确认全部执行，按推荐方案）

- [x] **音频**：重写 audio.rs 封装层（86d356b）。SoundKind 三轨路由 + BGM 流式 + crossfade，settings 三轨音量生效。
- [x] **文本**：text.rs 改用 cosmic-text Buffer/Layout（86d356b）。shaping/bidi/kerning/字符级回退，签名全兼容。
- [ ] **游戏内 UI**：渐进嵌入 iced 0.13 作运行时 UI。先迁控件型 UI（设置/存档/标题/目录选择器），保留对话框/立绘/打字机自绘。iced 0.13 共享 wgpu 22 无冲突，但 retained/immediate 混合需分层渲染。
- [ ] **编辑器 UI**：引入 iced_aw 扩展（menu/modal/sidebar/number_input/color_picker 替代部分手写弹窗）+ iced_glyphon 做构建日志终端。维持 iced 0.13。

## 工具链约束（勿破）

- Rust 1.92.0，wgpu 22，winit 0.29，iced 0.13
- **勿升 iced 0.14**（用 wgpu 27，与 akrs-render 的 wgpu 22 全项目冲突）
- akrs-editor 已在 workspace members 中，eframe/egui 已移除

## 临时文件（不上传）

- `.task4_notes.tmp` / `.task5_notes.tmp`：会话临时记录，git 忽略
