//! Ren'Py `.rpy` 文件到 `.akrs` 文件的转换。
//!
//! 仅转换可直接映射的语法，不支持的功能会产生警告。转换规则与 main 分支
//! egui 版本逐行一致。

use std::path::Path;

/// 将 Ren'Py .rpy 文件转换为 .akrs 文件,返回转换警告列表。
/// 转换失败(读/写)时返回 Err(错误描述)。
pub fn convert_rpy_to_akrs(source: &Path, target: &Path) -> Result<Vec<String>, String> {
    // 读取源文件
    let content = match std::fs::read_to_string(source) {
        Ok(c) => c,
        Err(e) => return Err(format!("无法读取源文件：{}", e)),
    };

    let mut akrs_lines: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    // 检测剧本主题（label/start）
    let has_label = content.contains("label start:") || content.contains("label start:");
    if !has_label {
        warnings.push("未检测到 'label start:'，可能不是标准的 Ren'Py 剧本文件".to_string());
    }

    // 逐行转换
    for line in content.lines() {
        let trimmed = line.trim();

        // 跳过空行和 Python 代码块
        if trimmed.is_empty() || trimmed.starts_with("python:") || trimmed.starts_with("$ ") {
            continue;
        }

        // label -> # 章节
        if trimmed.starts_with("label ") {
            let label_name = trimmed
                .strip_prefix("label ")
                .unwrap_or("")
                .trim_end_matches(':');
            akrs_lines.push(format!("# {}", label_name));
            continue;
        }

        // scene -> @bg
        if trimmed.starts_with("scene ") {
            let bg_name = trimmed
                .strip_prefix("scene ")
                .unwrap_or("")
                .split_whitespace()
                .next()
                .unwrap_or("");
            // Ren'Py 的 with 过渡（如 "with fade"）可尝试映射
            if trimmed.contains("with fade") {
                akrs_lines.push(format!("@bg {} with fade", bg_name));
            } else if trimmed.contains("with dissolve") {
                akrs_lines.push(format!("@bg {} with dissolve", bg_name));
            } else {
                akrs_lines.push(format!("@bg {}", bg_name));
            }
            continue;
        }

        // show -> + 立绘上场（简化映射）
        if trimmed.starts_with("show ") {
            let rest = trimmed.strip_prefix("show ").unwrap_or("");
            // 简化：取第一个词作为角色名
            let char_name = rest.split_whitespace().next().unwrap_or("");
            // Ren'Py 的 at 位置（如 "at left"）可映射
            if trimmed.contains("at left") {
                akrs_lines.push(format!("+ {} at left", char_name));
            } else if trimmed.contains("at right") {
                akrs_lines.push(format!("+ {} at right", char_name));
            } else if trimmed.contains("at center") {
                akrs_lines.push(format!("+ {}", char_name));
            } else {
                akrs_lines.push(format!("+ {}", char_name));
            }
            continue;
        }

        // hide -> - 立绘下场
        if trimmed.starts_with("hide ") {
            let char_name = trimmed
                .strip_prefix("hide ")
                .unwrap_or("")
                .split_whitespace()
                .next()
                .unwrap_or("");
            akrs_lines.push(format!("- {}", char_name));
            continue;
        }

        // 对话 "说话人 \"对话内容\""
        if trimmed.starts_with('"') && trimmed.contains("\" \"") {
            // 格式如: "说话人 \"对话\""
            // 简化处理：提取说话人和对话
            let parts: Vec<&str> = trimmed.splitn(2, "\" \"").collect();
            if parts.len() == 2 {
                let speaker = parts[0].trim_start_matches('"').trim();
                let dialogue = parts[1].trim_end_matches('"').trim();
                akrs_lines.push(format!("{}: \"{}\"", speaker, dialogue));
                continue;
            }
        }

        // narrate 旁白（无说话人的字符串）
        if trimmed.starts_with('"') && trimmed.ends_with('"') {
            let narration = trimmed.trim_matches('"');
            akrs_lines.push(format!("\"{}\"", narration));
            continue;
        }

        // menu -> ? 选择分支
        if trimmed.starts_with("menu:") {
            akrs_lines.push("? \"\"".to_string());
            continue;
        }

        // 菜单选项（以字符串开头后冒号）-> | 选项
        if trimmed.starts_with('"') && trimmed.contains(':') && !trimmed.contains("\" \"") {
            // 格式如: "选项文本":（后接 jump）
            let parts: Vec<&str> = trimmed.splitn(2, ':').collect();
            if parts.len() == 2 {
                let option_text = parts[0].trim_matches('"').trim();
                akrs_lines.push(format!("| \"{}\"", option_text));
                // 如果有 jump，映射为 ->
                let rest = parts[1].trim();
                if rest.starts_with("jump ") {
                    let target_label = rest.strip_prefix("jump ").unwrap_or("").trim();
                    akrs_lines.push(format!("    -> {}", target_label));
                }
                continue;
            }
        }

        // return / jump -> -> 或结束
        if trimmed.starts_with("return") {
            akrs_lines.push("~~".to_string());
            continue;
        }
        if trimmed.starts_with("jump ") {
            let target = trimmed.strip_prefix("jump ").unwrap_or("").trim();
            akrs_lines.push(format!("-> {}", target));
            continue;
        }

        // 不支持的 Ren'Py 特性产生警告
        if trimmed.starts_with("play music")
            || trimmed.starts_with("play sound")
            || trimmed.starts_with("stop music")
            || trimmed.starts_with("stop sound")
        {
            warnings.push(format!("音频播放指令未转换：{}", trimmed));
            continue;
        }
        if trimmed.starts_with("with ") {
            warnings.push(format!("独立过渡指令未转换：{}", trimmed));
            continue;
        }
        if trimmed.starts_with("call ")
            || trimmed.starts_with("if ")
            || trimmed.starts_with("while ")
            || trimmed.starts_with("for ")
        {
            warnings.push(format!("复杂流程控制未转换：{}", trimmed));
            continue;
        }
        if trimmed.starts_with("define ")
            || trimmed.starts_with("default ")
            || trimmed.starts_with("init ")
        {
            warnings.push(format!("定义/初始化块未转换：{}", trimmed));
            continue;
        }
        if trimmed.starts_with("image ")
            || trimmed.starts_with("transform ")
            || trimmed.starts_with("animation ")
        {
            warnings.push(format!("图像/动画定义未转换：{}", trimmed));
            continue;
        }
        if trimmed.contains("renpy.")
            || trimmed.contains("Ren'Py")
        {
            warnings.push(format!("Ren'Py 内置函数/变量未转换：{}", trimmed));
            continue;
        }
        // 注释行保留
        if trimmed.starts_with('#') {
            akrs_lines.push(format!("// {}", trimmed.trim_start_matches('#').trim()));
            continue;
        }

        // 未识别的行保留为注释
        if !trimmed.is_empty() {
            akrs_lines.push(format!("// 未转换: {}", trimmed));
            warnings.push(format!("未识别的行：{}", trimmed));
        }
    }

    // 构建结果
    let akrs_content = akrs_lines.join("\n");

    // 确保目标目录存在
    if let Some(parent) = target.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // 写入目标文件
    if let Err(e) = std::fs::write(target, &akrs_content) {
        return Err(format!("写入失败：{}", e));
    }

    // 无警告时补充一条完成提示（与 main 行为一致）。
    if warnings.is_empty() {
        warnings.push("转换完成，无警告".to_string());
    }
    Ok(warnings)
}
