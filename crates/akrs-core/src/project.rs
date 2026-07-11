use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 项目配置持久化错误。
///
/// 用 `thiserror` 派生 `Error`/`Display`，替代原先的 `Result<(), String>`。
#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    /// 序列化 project.json 失败（serde_json 错误）。
    #[error("序列化失败：{0}")]
    Serialize(serde_json::Error),
    /// 写入 project.json 失败（IO 错误）。
    #[error("写入失败：{0}")]
    Write(std::io::Error),
}

/// 项目配置：存储游戏项目的所有个性化设置，
/// 包括标题、作者、窗口设置、默认语言、版本号等。
///
/// 保存为项目根目录下的 `project.json` 文件。
/// 所有新增字段均带 `#[serde(default)]`，确保旧项目文件可正常加载。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectConfig {
    /// 主标题（标题页显示）
    pub title: String,
    /// 副标题（标题页显示）
    pub subtitle: String,
    /// 项目描述（可选，编辑器中显示）
    #[serde(default)]
    pub description: String,
    /// 作者（可选）
    #[serde(default)]
    pub author: String,
    /// 主剧本文件路径（相对于项目根目录）
    #[serde(default = "default_script_path")]
    pub main_script: String,
    /// OS 窗口标题栏显示的文字。若为空则回退到 title。
    #[serde(default)]
    pub window_title: String,
    /// 启动时是否全屏。
    #[serde(default)]
    pub start_fullscreen: bool,
    /// 默认窗口分辨率（宽，高），默认 1920×1080。
    #[serde(default = "default_resolution")]
    pub default_resolution: (u32, u32),
    /// 项目默认语言代码（如 "zh-CN"、"en-US"）。
    /// 玩家首次启动时使用此语言，之后以玩家设置为准。
    #[serde(default = "default_language")]
    pub language: String,
    /// 项目版本号。
    #[serde(default = "default_version")]
    pub version: String,
    /// 开屏页（标题页）背景图片资源名。
    /// 相对 `assets/` 目录（如 `title.png` 或 `title/my_opening.png`）。
    /// 留空则回退到默认的 `title.png`。
    #[serde(default)]
    pub title_background: String,
    /// 开屏页（标题页）背景音乐资源名。
    /// 相对 `assets/music/` 目录（如 `title_bgm.mp3`）。
    /// 留空则回退到默认的 `title_bgm.mp3`；若该文件也不存在则静音。
    #[serde(default)]
    pub title_music: String,
    /// 游戏主题配色（4 种可自定义颜色）。
    /// 各字段带 `#[serde(default)]`，旧项目文件缺失时回退内置配色。
    #[serde(default)]
    pub theme: ThemeColors,
    /// 项目警告：用编辑器打开本项目时弹出的作者提示文字（彩蛋/版权声明等）。
    /// 留空则不弹出。玩家可在弹窗里选「不再显示」（仅本地生效，存于编辑器数据目录）。
    #[serde(default)]
    pub warning: String,
}

/// 游戏主题配色：RGBA 通道均为 0–255。
///
/// - `primary`：主题色1，模态对话框/面板背景（默认深蓝紫）。
/// - `secondary`：主题色2，按钮背景基色（默认亮蓝），悬停/按下态由代码派生。
/// - `text`：文本色，按钮/HUD 文字（默认白）。
/// - `dialogue`：对话框色，游戏进行中文本框渐变基色（默认浅蓝）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeColors {
    #[serde(default = "default_theme_primary")]
    pub primary: [u8; 4],
    #[serde(default = "default_theme_secondary")]
    pub secondary: [u8; 4],
    #[serde(default = "default_theme_text")]
    pub text: [u8; 4],
    #[serde(default = "default_theme_dialogue")]
    pub dialogue: [u8; 4],
}

fn default_theme_primary() -> [u8; 4] { [20, 15, 38, 247] }
fn default_theme_secondary() -> [u8; 4] { [77, 140, 217, 230] }
fn default_theme_text() -> [u8; 4] { [255, 255, 255, 255] }
fn default_theme_dialogue() -> [u8; 4] { [140, 199, 242, 255] }

impl Default for ThemeColors {
    fn default() -> Self {
        Self {
            primary: default_theme_primary(),
            secondary: default_theme_secondary(),
            text: default_theme_text(),
            dialogue: default_theme_dialogue(),
        }
    }
}

impl ThemeColors {
    /// 从 `[u8;4]` 派生 macroquad 风格的 f32 颜色（0.0–1.0）。
    /// 供渲染层使用，避免在每个绘制函数里重复除以 255。
    pub fn to_f32(&self) -> ([f32; 4], [f32; 4], [f32; 4], [f32; 4]) {
        let c = |v: [u8; 4]| {
            [v[0] as f32 / 255.0, v[1] as f32 / 255.0, v[2] as f32 / 255.0, v[3] as f32 / 255.0]
        };
        (c(self.primary), c(self.secondary), c(self.text), c(self.dialogue))
    }
}

fn default_script_path() -> String {
    "main.akrs".to_string()
}

fn default_resolution() -> (u32, u32) {
    (1920, 1080)
}

fn default_language() -> String {
    "zh-CN".to_string()
}

fn default_version() -> String {
    "0.1.0".to_string()
}

impl Default for ProjectConfig {
    fn default() -> Self {
        Self {
            title: "Akizuki*Rustgal".to_string(),
            subtitle: "夏夜观心Extra".to_string(),
            description: String::new(),
            author: String::new(),
            main_script: "main.akrs".to_string(),
            window_title: String::new(),
            start_fullscreen: false,
            default_resolution: (1920, 1080),
            language: "zh-CN".to_string(),
            version: "0.1.0".to_string(),
            title_background: String::new(),
            title_music: String::new(),
            theme: ThemeColors::default(),
            warning: String::new(),
        }
    }
}

impl ProjectConfig {
    /// 从项目目录加载 project.json，如果不存在则返回默认值。
    pub fn load(project_dir: &Path) -> Self {
        let path = project_dir.join("project.json");
        if let Ok(content) = std::fs::read_to_string(&path) {
            serde_json::from_str(&content).unwrap_or_else(|_| Self::default())
        } else {
            Self::default()
        }
    }

    /// 保存到项目目录下的 project.json。
    pub fn save(&self, project_dir: &Path) -> Result<(), ProjectError> {
        let path = project_dir.join("project.json");
        let content = serde_json::to_string_pretty(self).map_err(ProjectError::Serialize)?;
        std::fs::write(&path, content).map_err(ProjectError::Write)?;
        Ok(())
    }

    /// 检查主标题是否过长（超过屏幕宽度的 1/4 作为警告阈值）。
    /// 这里只做基于字符数的粗略估算（中文约 1 字 = 1em，英文约 0.5em）。
    pub fn is_title_too_long(&self) -> bool {
        estimated_width(&self.title) > 12.0
    }

    /// 检查副标题是否过长。
    pub fn is_subtitle_too_long(&self) -> bool {
        estimated_width(&self.subtitle) > 12.0
    }

    /// 获取有效窗口标题：window_title 非空则使用它，否则回退到 title。
    pub fn effective_window_title(&self) -> &str {
        if self.window_title.is_empty() {
            &self.title
        } else {
            &self.window_title
        }
    }
}

/// 粗略估算文本宽度（单位：em，即 1em = 1 个中文字符宽度）。
fn estimated_width(text: &str) -> f32 {
    let mut w = 0.0;
    for c in text.chars() {
        if c.is_ascii() {
            w += 0.55;
        } else {
            w += 1.0;
        }
    }
    w
}

/// 最近打开的项目记录。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RecentProjects {
    pub projects: Vec<RecentProject>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentProject {
    pub path: PathBuf,
    pub name: String,
    pub last_opened: u64,
}

impl RecentProjects {
    const MAX_RECENT: usize = 10;

    pub fn load() -> Self {
        let path = recent_projects_path();
        if let Ok(content) = std::fs::read_to_string(&path) {
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            Self::default()
        }
    }

    pub fn save(&self) {
        let path = recent_projects_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(content) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(&path, content);
        }
    }

    pub fn add_project(&mut self, path: &Path, name: &str) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        // 移除已存在的同路径项目
        self.projects.retain(|p| p.path != path);

        // 插入到最前面
        self.projects.insert(0, RecentProject {
            path: path.to_path_buf(),
            name: name.to_string(),
            last_opened: now,
        });

        // 保留最近 N 个
        if self.projects.len() > Self::MAX_RECENT {
            self.projects.truncate(Self::MAX_RECENT);
        }

        self.save();
    }
}

fn recent_projects_path() -> PathBuf {
    if let Some(data_dir) = dirs_data_dir() {
        data_dir.join("akrs-editor").join("recent_projects.json")
    } else {
        PathBuf::from(".akrs_recent.json")
    }
}

fn dirs_data_dir() -> Option<PathBuf> {
    // 使用 dirs crate 做跨平台数据目录解析，替代手写的 40 行环境变量逻辑。
    // 修复了旧实现的 bug：Linux 下 ~/.local/share 不存在时回退到家目录根（污染）。
    dirs::data_dir().or_else(dirs::data_local_dir)
}

/// 本地「不再显示」的项目警告忽略列表。
///
/// 存储于编辑器数据目录下的 `dismissed_warnings.json`，内容为已被玩家
/// 点击「不再显示」的项目规范化路径集合。仅本地生效，不随项目分发。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DismissedWarnings {
    pub paths: Vec<String>,
}

impl DismissedWarnings {
    /// 从编辑器数据目录加载忽略列表。
    pub fn load() -> Self {
        if let Some(path) = dismissed_warnings_path() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                return serde_json::from_str(&content).unwrap_or_default();
            }
        }
        Self::default()
    }

    /// 保存忽略列表到编辑器数据目录。
    pub fn save(&self) {
        if let Some(path) = dismissed_warnings_path() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(content) = serde_json::to_string_pretty(self) {
                let _ = std::fs::write(&path, content);
            }
        }
    }

    /// 判断指定项目目录是否已被本地忽略。
    /// 用规范化路径比较，避免相对路径/软链接差异。
    pub fn is_dismissed(&self, project_dir: &Path) -> bool {
        let key = match canonical_key(project_dir) {
            Some(k) => k,
            None => return false,
        };
        self.paths.iter().any(|p| *p == key)
    }

    /// 将项目目录加入本地忽略列表并持久化。
    pub fn dismiss(&mut self, project_dir: &Path) {
        if let Some(key) = canonical_key(project_dir) {
            if !self.paths.iter().any(|p| *p == key) {
                self.paths.push(key);
                self.save();
            }
        }
    }
}

fn dismissed_warnings_path() -> Option<PathBuf> {
    dirs_data_dir().map(|d| d.join("akrs-editor").join("dismissed_warnings.json"))
}

/// 取项目目录的规范化路径字符串作为忽略列表 key。
/// 规范化失败时回退到传入路径的 lossy 字符串。
fn canonical_key(project_dir: &Path) -> Option<String> {
    match std::fs::canonicalize(project_dir) {
        Ok(p) => Some(p.to_string_lossy().into_owned()),
        Err(_) => Some(project_dir.to_string_lossy().into_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warning_field_deserializes_when_present() {
        let json = r#"{
            "title": "T",
            "subtitle": "S",
            "warning": "版权声明"
        }"#;
        let cfg: ProjectConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.warning, "版权声明");
        // 缺省字段走默认值
        assert_eq!(cfg.main_script, "main.akrs");
    }

    #[test]
    fn warning_field_defaults_empty_when_absent() {
        // 旧项目文件不含 warning 字段时，应回退为空字符串而非报错。
        let json = r#"{
            "title": "T",
            "subtitle": "S"
        }"#;
        let cfg: ProjectConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.warning, "");
    }

    #[test]
    fn dismissed_warnings_is_not_initially_dismissed() {
        let dw = DismissedWarnings::default();
        assert!(!dw.is_dismissed(Path::new("/nonexistent/project")));
    }
}
