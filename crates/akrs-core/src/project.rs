use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 项目配置：存储游戏项目的元数据，如标题、副标题等。
///
/// 保存为项目根目录下的 `project.json` 文件。
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
}

fn default_script_path() -> String {
    "main.akrs".to_string()
}

impl Default for ProjectConfig {
    fn default() -> Self {
        Self {
            title: "Akizuki*Rustgal".to_string(),
            subtitle: "夏夜观心Extra".to_string(),
            description: String::new(),
            author: String::new(),
            main_script: "main.akrs".to_string(),
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
    pub fn save(&self, project_dir: &Path) -> Result<(), String> {
        let path = project_dir.join("project.json");
        let content = serde_json::to_string_pretty(self)
            .map_err(|e| format!("序列化失败：{}", e))?;
        std::fs::write(&path, content)
            .map_err(|e| format!("写入失败：{}", e))?;
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
    // 简单实现：优先使用 ~/.local/share，回退到当前目录
    if let Ok(home) = std::env::var("HOME") {
        let path = PathBuf::from(&home).join(".local").join("share");
        if path.exists() {
            return Some(path);
        }
        return Some(PathBuf::from(home));
    }
    None
}
