//! 编辑器常量与模板。
//!
//! 汇集编辑器使用的字号、章节标题长度上限、新建/示例剧本模板、GitHub 链接、
//! 撤销参数、build 号，以及蓝图模式可选的节点模板列表。

/// 编辑器正文字号。
pub const FONT_SIZE: f32 = 14.0;

/// 章节显示标题（`# name title` 中的 title，无标题时取 name）的最大建议字符数。
/// 超过此值时编辑器给出警告（非阻断，仍可强制运行；运行时通知会自动缩字显示）。
/// 取值依据：顶部章节通知宽度约为屏幕 80%，基准字号下可舒适显示约 24 个字符。
pub const MAX_CHAPTER_TITLE_CHARS: usize = 24;

/// 「新建」操作使用的小型有效模板。
pub const NEW_TEMPLATE: &str = "# Start\n\n~~\n";

/// 首次启动或点击「打开示例剧本」时加载的丰富示例。
pub const SAMPLE_SCRIPT: &str = r#"# Start

@bg school with fade
+ Aki enters from left with dissolve

"Cherry blossoms drift through the air."

Aki: "Hello there!"
Aki (happy): "I'm glad you came."

$affection = 1

? "What do you say?"
| "You're wonderful!"
    $affection += 3
    -> GoodEnding
| "Whatever."
    $affection -= 1
    -> BadEnding
?

# GoodEnding

@bg sunset with fade_white

Aki: "I think we'll be great friends."

~~



# BadEnding

Aki: "Oh. I see."

~~
"#;

/// GitHub 仓库链接。
pub const GITHUB_URL: &str = "https://github.com/AkizukiKokona/Akizuki-Rustgal";

/// 撤销历史栈最大保留条数。超过此值时丢弃最旧的快照。
pub const UNDO_LIMIT: usize = 100;

/// 编辑空闲合并阈值（秒）：连续输入在该时长内不提交新历史条目，
/// 超过该时长后下一次提交视为新的可撤销步骤。
pub const UNDO_IDLE_SECS: f32 = 0.6;

/// 关于页面显示的 build 号。本轮递增一次（0091 → 0092）。
pub const BUILD_NUMBER: &str = "0092";

/// 蓝图右键菜单中可选的节点模板：(标签, 简短描述, 模板文本)。
pub const NODE_TEMPLATES: &[(&str, &str, &str)] = &[
    ("章节", "章节标题分隔", "# NewSection 章节标题"),
    ("对话", "角色说话", "角色: \"对话内容\""),
    ("旁白", "叙述文字", "\"旁白内容\""),
    ("背景", "切换背景图", "@bg background"),
    ("音乐", "播放音乐", "@music music"),
    ("立绘上场", "角色立绘登场", "+ 角色 at 0.5,1.0 size 1.0"),
    ("立绘下场", "角色立绘退场", "- 角色"),
    ("选择", "分支选项", "? 提示\n| 选项A\n| 选项B\n?"),
    ("跳转", "跳转到章节", "-> TargetSection"),
    ("访问", "访问子章节", "=> TargetSection"),
    ("返回", "从子章节返回", "<="),
    ("等待", "暂停若干秒", "~~ 1.0"),
    ("结局", "故事结束", "end"),
];
