//! Visual script editor for the Akizuki*Rustgal engine.
//!
//! Built with `egui 0.21` + `eframe 0.21`. Provides a three-panel layout:
//!
//! - **Left panel**: file list of `.akrs` files in the working directory, with
//!   new / open / save operations.
//! - **Center panel**: multiline script editor with basic syntax highlighting.
//! - **Right panel**: live preview powered by `akrs_runtime::Engine`.
//! - **Top toolbar**: New / Open / Save / Run buttons.
//! - **Bottom status bar**: compile diagnostics (errors / warnings / notes).
//!
//! All file operations use `Result`-style error handling and never panic; any
//! failure is reported in the bottom status bar.

use std::path::PathBuf;

use eframe::egui;

use akrs_core::{compile, format_location, CompileError, ErrSeverity, Position};
use akrs_runtime::{Engine, EnginePhase};

// ---------------------------------------------------------------------------
// Syntax highlighting color palette (opaque RGB, derived from the spec's RGBA).
// ---------------------------------------------------------------------------

/// `#` section headers -> (0.9, 0.8, 1.0)
const COLOR_SECTION: egui::Color32 = egui::Color32::from_rgb(229, 204, 255);
/// `->` `=>` `<=` `~~` flow control -> (1.0, 0.6, 0.3)
const COLOR_FLOW: egui::Color32 = egui::Color32::from_rgb(255, 153, 76);
/// `@` commands -> (0.3, 0.8, 0.3)
const COLOR_COMMAND: egui::Color32 = egui::Color32::from_rgb(76, 204, 76);
/// `+` `-` character directions -> (0.3, 0.7, 1.0)
const COLOR_DIRECTION: egui::Color32 = egui::Color32::from_rgb(76, 178, 255);
/// `$` variable ops -> (1.0, 0.8, 0.3)
const COLOR_VARIABLE: egui::Color32 = egui::Color32::from_rgb(255, 204, 76);
/// `?` `|` choice blocks -> (0.8, 0.3, 0.8)
const COLOR_CHOICE: egui::Color32 = egui::Color32::from_rgb(204, 76, 204);
/// `--` comments -> (0.4, 0.4, 0.4)
const COLOR_COMMENT: egui::Color32 = egui::Color32::from_rgb(102, 102, 102);
/// `"..."` strings -> (0.9, 0.9, 0.4)
const COLOR_STRING: egui::Color32 = egui::Color32::from_rgb(229, 229, 102);
/// default text -> white
const COLOR_DEFAULT: egui::Color32 = egui::Color32::from_rgb(255, 255, 255);

const FONT_SIZE: f32 = 14.0;

/// A small valid template used by the "New" action.
const NEW_TEMPLATE: &str = "# Start\n\n~~\n";

/// A richer sample loaded on first launch so the editor is never empty.
const SAMPLE_SCRIPT: &str = r#"# Start

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

// ---------------------------------------------------------------------------
// Editor application state
// ---------------------------------------------------------------------------

/// The egui application backing the editor.
pub struct EditorApp {
    /// Current script text shown in the center editor.
    editor_content: String,
    /// Path of the file currently loaded/saved (`None` when unsaved).
    current_file: Option<PathBuf>,
    /// Directory scanned for the file list and used for save/open.
    work_dir: PathBuf,
    /// Name of the file to open/save (edited in the left panel).
    file_name_input: String,
    /// Cached list of `.akrs` files in `work_dir`.
    file_list: Vec<String>,
    /// Running preview engine (created by "Run").
    engine: Option<Engine>,
    /// Human-readable status line shown at the bottom.
    status: String,
    /// Formatted compile diagnostics (errors / warnings / notes).
    diagnostics: Vec<String>,
    /// Last frame's time (seconds), used to derive a delta for the engine.
    last_time: f64,
    /// Whether the dark theme has been applied yet.
    theme_applied: bool,
}

impl Default for EditorApp {
    fn default() -> Self {
        let work_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let mut app = Self {
            editor_content: SAMPLE_SCRIPT.to_string(),
            current_file: None,
            work_dir,
            file_name_input: "untitled.akrs".to_string(),
            file_list: Vec::new(),
            engine: None,
            status: "Ready".to_string(),
            diagnostics: Vec::new(),
            last_time: 0.0,
            theme_applied: false,
        };
        app.refresh_file_list();
        app
    }
}

impl EditorApp {
    // -- File operations (all fallible, never panic) ------------------------

    /// Re-scan `work_dir` for `.akrs` files.
    fn refresh_file_list(&mut self) {
        self.file_list.clear();
        if let Ok(entries) = std::fs::read_dir(&self.work_dir) {
            let mut files: Vec<String> = entries
                .filter_map(|e| e.ok())
                .filter_map(|e| {
                    let p = e.path();
                    if p.extension().is_some_and(|ext| ext == "akrs") {
                        p.file_name().map(|n| n.to_string_lossy().into_owned())
                    } else {
                        None
                    }
                })
                .collect();
            files.sort();
            self.file_list = files;
        }
    }

    /// Start a new (unsaved) script from a minimal template.
    fn new_file(&mut self) {
        self.editor_content = NEW_TEMPLATE.to_string();
        self.current_file = None;
        self.file_name_input = "untitled.akrs".to_string();
        self.engine = None;
        self.diagnostics.clear();
        self.status = "New file (unsaved)".to_string();
    }

    /// Open `name` from `work_dir` into the editor.
    fn open_file(&mut self, name: &str) {
        let path = self.work_dir.join(name);
        match std::fs::read_to_string(&path) {
            Ok(content) => {
                self.editor_content = content;
                self.current_file = Some(path);
                self.file_name_input = name.to_string();
                self.engine = None;
                self.diagnostics.clear();
                self.status = format!("Opened {}", name);
            }
            Err(e) => {
                self.status = format!("Failed to open {}: {}", name, e);
            }
        }
    }

    /// Open whatever name is currently in `file_name_input`.
    fn open_current_name(&mut self) {
        let name = sanitize_filename(&self.file_name_input);
        self.open_file(&name);
    }

    /// Save the editor content to `work_dir/file_name_input`.
    fn save_file(&mut self) {
        let name = sanitize_filename(&self.file_name_input);
        let path = self.work_dir.join(&name);
        match std::fs::write(&path, &self.editor_content) {
            Ok(()) => {
                self.current_file = Some(path);
                self.file_name_input = name.clone();
                self.status = format!("Saved {}", name);
                self.refresh_file_list();
            }
            Err(e) => {
                self.status = format!("Failed to save {}: {}", name, e);
            }
        }
    }

    // -- Compilation + preview ----------------------------------------------

    /// Compile the current editor content and (on success) start a preview
    /// engine. Diagnostics are always reflected in the status bar.
    fn run_script(&mut self) {
        let (program, errors) = compile(&self.editor_content);
        self.diagnostics = format_errors(&errors);

        match program {
            Some(_) => match Engine::start_running(&self.editor_content) {
                Ok(mut engine) => {
                    // Instant text in the preview so dialogue is readable.
                    engine.settings_mut().text_speed = 999.0;
                    self.engine = Some(engine);
                    let n = self.diagnostics.len();
                    if n == 0 {
                        self.status = "Running".to_string();
                    } else {
                        self.status = format!("Running ({} diagnostic(s))", n);
                    }
                }
                Err(errs) => {
                    // Defensive: `compile` returned a program, so this should
                    // not happen, but surface it regardless.
                    self.diagnostics = format_errors(&errs);
                    self.engine = None;
                    self.status = "Engine failed to start".to_string();
                }
            },
            None => {
                self.engine = None;
                let n = self
                    .diagnostics
                    .iter()
                    .filter(|d| d.starts_with("[error]"))
                    .count();
                self.status = format!("Compile failed ({} error(s))", n);
            }
        }
    }

    // -- Panel rendering ----------------------------------------------------

    fn show_editor(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let mut layouter = |ui: &egui::Ui, string: &str, wrap_width: f32| {
                    let mut job = highlight_code(string);
                    job.wrap.max_width = wrap_width;
                    ui.fonts(|f| f.layout_job(job))
                };
                ui.add(
                    egui::TextEdit::multiline(&mut self.editor_content)
                        .code_editor()
                        .desired_width(f32::MAX)
                        .layouter(&mut layouter),
                );
            });
    }

    fn show_preview(&mut self, ui: &mut egui::Ui) {
        // Snapshot the scene so choice buttons can mutate the engine below
        // without holding an immutable borrow of `self.engine`.
        let (phase, scene) = match self.engine.as_ref() {
            Some(engine) => (engine.phase(), engine.scene().clone()),
            None => {
                ui.label("Click \"Run\" to preview the script.");
                return;
            }
        };

        ui.label(format!("Phase: {}", phase_label(phase)));
        ui.add_space(4.0);

        ui.strong("Background");
        match &scene.background {
            Some(bg) => ui.label(format!("name = {}", bg.name)),
            None => ui.label("(none)"),
        };
        ui.add_space(4.0);

        ui.strong("Characters");
        if scene.characters.is_empty() {
            ui.label("(none on stage)");
        } else {
            for c in &scene.characters {
                ui.label(format!("- {} [{}]", c.name, position_label(&c.position)));
            }
        }
        ui.add_space(4.0);

        ui.strong("Dialogue");
        match &scene.dialogue {
            Some(d) => {
                if d.speaker.is_empty() {
                    ui.label(egui::RichText::new("(narration)").italics());
                } else {
                    ui.strong(d.speaker.as_str());
                }
                let shown: String = d.full_text.chars().take(d.displayed_chars).collect();
                ui.label(shown);
                ui.label(format!(
                    "({}/{} chars, complete={})",
                    d.displayed_chars,
                    d.full_text.chars().count(),
                    d.complete
                ));
            }
            None => {
                ui.label("(none)");
            }
        };
        ui.add_space(4.0);

        ui.strong("Choices");
        match &scene.choices {
            Some(ch) => {
                if let Some(p) = &ch.prompt {
                    ui.label(format!("prompt: {}", p));
                }
                if ch.options.is_empty() {
                    ui.label("(no options)");
                }
                for (i, opt) in ch.options.iter().enumerate() {
                    let label = format!(
                        "{}. {}{}",
                        i + 1,
                        opt.text,
                        if opt.available { "" } else { " (disabled)" }
                    );
                    if ui.button(label).clicked() {
                        if let Some(engine) = self.engine.as_mut() {
                            let _ = engine.choose(i);
                        }
                    }
                }
            }
            None => {
                ui.label("(none)");
            }
        };

        if scene.story_ended {
            ui.add_space(4.0);
            ui.colored_label(
                egui::Color32::from_rgb(255, 150, 150),
                "Story ended.",
            );
        }
    }
}

impl eframe::App for EditorApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.theme_applied {
            ctx.set_visuals(egui::Visuals::dark());
            self.theme_applied = true;
        }

        // Frame delta for engine animation (typewriter / transitions).
        let now: f64 = ctx.input(|i| i.time);
        let dt = if self.last_time > 0.0 {
            ((now - self.last_time) as f32).clamp(0.0, 0.1)
        } else {
            0.0
        };
        self.last_time = now;

        if let Some(engine) = self.engine.as_mut() {
            let _ = engine.update(dt);
        }
        // Keep animating while the engine is mid-transition / waiting or the
        // typewriter has not finished revealing the current line.
        if let Some(engine) = &self.engine {
            let animating = matches!(
                engine.phase(),
                EnginePhase::Transitioning | EnginePhase::Waiting
            ) || engine
                .scene()
                .dialogue
                .as_ref()
                .is_some_and(|d| !d.complete);
            if animating {
                ctx.request_repaint();
            }
        }

        // ---- Top toolbar --------------------------------------------------
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.button("New").clicked() {
                    self.new_file();
                }
                if ui.button("Open").clicked() {
                    self.open_current_name();
                }
                if ui.button("Save").clicked() {
                    self.save_file();
                }
                ui.separator();
                if ui.button("Run").clicked() {
                    self.run_script();
                }
                ui.separator();
                ui.label(format!("File: {}", self.file_name_input));
            });
        });

        // ---- Bottom status bar -------------------------------------------
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!("Status: {}", self.status));
                ui.separator();
                ui.label(format!("Diagnostics: {}", self.diagnostics.len()));
            });
            if !self.diagnostics.is_empty() {
                ui.separator();
                let shown = self.diagnostics.len().min(8);
                for msg in self.diagnostics.iter().take(shown) {
                    let color = if msg.starts_with("[error]") {
                        egui::Color32::from_rgb(255, 120, 120)
                    } else if msg.starts_with("[warning]") {
                        egui::Color32::from_rgb(220, 200, 120)
                    } else {
                        egui::Color32::from_rgb(150, 170, 200)
                    };
                    ui.label(egui::RichText::new(msg).color(color).monospace());
                }
                if self.diagnostics.len() > shown {
                    ui.label(format!("...and {} more", self.diagnostics.len() - shown));
                }
            }
        });

        // ---- Left panel: file list ---------------------------------------
        egui::SidePanel::left("files")
            .resizable(true)
            .default_width(210.0)
            .show(ctx, |ui| {
                ui.heading("Files");
                ui.label(format!("Dir: {}", self.work_dir.display()));
                ui.horizontal(|ui| {
                    ui.label("Name:");
                    ui.text_edit_singleline(&mut self.file_name_input);
                });
                ui.horizontal(|ui| {
                    if ui.button("New").clicked() {
                        self.new_file();
                    }
                    if ui.button("Save").clicked() {
                        self.save_file();
                    }
                    if ui.button("Refresh").clicked() {
                        self.refresh_file_list();
                    }
                });
                ui.separator();
                ui.label("Open a file:");
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if self.file_list.is_empty() {
                        ui.label("(no .akrs files)");
                    }
                    // Clone so we can mutate `self` while iterating.
                    let files = self.file_list.clone();
                    for name in &files {
                        let selected = self
                            .current_file
                            .as_ref()
                            .and_then(|p| p.file_name())
                            .is_some_and(|n| n == name.as_str());
                        if ui.selectable_label(selected, name.as_str()).clicked() {
                            self.open_file(name);
                        }
                    }
                });
            });

        // ---- Right panel: preview ----------------------------------------
        egui::SidePanel::right("preview")
            .resizable(true)
            .default_width(330.0)
            .show(ctx, |ui| {
                ui.heading("Preview");
                ui.horizontal(|ui| {
                    if ui.button("Run").clicked() {
                        self.run_script();
                    }
                    if self.engine.is_some() {
                        if ui.button("Advance").clicked() {
                            if let Some(engine) = self.engine.as_mut() {
                                let _ = engine.advance();
                            }
                        }
                        if ui.button("Stop").clicked() {
                            self.engine = None;
                            self.status = "Preview stopped".to_string();
                        }
                    }
                });
                ui.separator();
                self.show_preview(ui);
            });

        // ---- Central panel: editor ---------------------------------------
        egui::CentralPanel::default().show(ctx, |ui| {
            self.show_editor(ui);
        });
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Launch the GUI editor application.
pub fn run_editor() -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions {
        initial_window_size: Some(egui::Vec2::new(1280.0, 820.0)),
        ..Default::default()
    };
    eframe::run_native(
        "Akizuki*Rustgal Editor",
        options,
        Box::new(|_cc| Box::new(EditorApp::default())),
    )
}

// ---------------------------------------------------------------------------
// Helpers: filename sanitization, diagnostics, labels
// ---------------------------------------------------------------------------

/// Normalize a user-typed filename: strip path separators and ensure the
/// `.akrs` extension is present.
fn sanitize_filename(input: &str) -> String {
    let mut name: String = input
        .trim()
        .chars()
        .filter(|c| !matches!(c, '/' | '\\'))
        .collect();
    if name.is_empty() {
        name = "untitled.akrs".to_string();
    }
    if !name.ends_with(".akrs") {
        name.push_str(".akrs");
    }
    name
}

/// Format compile diagnostics into single-line strings tagged by severity.
fn format_errors(errors: &[CompileError]) -> Vec<String> {
    errors
        .iter()
        .map(|e| {
            let sev = match e.severity {
                ErrSeverity::Error => "error",
                ErrSeverity::Warning => "warning",
                ErrSeverity::Note => "note",
            };
            let loc = format_location(&e.span);
            match &e.hint {
                Some(h) => format!("[{}] {} - at {} (hint: {})", sev, e.message, loc, h),
                None => format!("[{}] {} - at {}", sev, e.message, loc),
            }
        })
        .collect()
}

/// Human-readable name for an engine phase.
fn phase_label(phase: EnginePhase) -> &'static str {
    match phase {
        EnginePhase::Title => "Title",
        EnginePhase::Running => "Running",
        EnginePhase::Transitioning => "Transitioning",
        EnginePhase::Waiting => "Waiting",
        EnginePhase::ChoicePending => "ChoicePending",
        EnginePhase::StoryEnded => "StoryEnded",
    }
}

/// Human-readable label for a character position.
fn position_label(p: &Position) -> String {
    match p {
        Position::Left => "Left".to_string(),
        Position::Center => "Center".to_string(),
        Position::Right => "Right".to_string(),
        Position::Custom(x) => format!("Custom({:.2})", x),
    }
}

// ---------------------------------------------------------------------------
// Syntax highlighting
// ---------------------------------------------------------------------------

/// Build a `LayoutJob` with per-token coloring for the whole buffer.
fn highlight_code(text: &str) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let lines: Vec<&str> = text.split('\n').collect();
    for (idx, line) in lines.iter().enumerate() {
        highlight_line(&mut job, line);
        if idx + 1 < lines.len() {
            // Re-insert the newline that `split` consumed.
            job.append("\n", 0.0, text_format(COLOR_DEFAULT));
        }
    }
    job
}

/// Choose the base color for a line based on its first non-whitespace token.
fn line_base_color(trimmed: &str) -> egui::Color32 {
    // Two-char markers starting with `-` must be checked before the single
    // `-` direction marker.
    if trimmed.starts_with('#') {
        COLOR_SECTION
    } else if trimmed.starts_with("--") {
        COLOR_COMMENT
    } else if trimmed.starts_with("->")
        || trimmed.starts_with("=>")
        || trimmed.starts_with("<=")
        || trimmed.starts_with("~~")
    {
        COLOR_FLOW
    } else if trimmed.starts_with('@') {
        COLOR_COMMAND
    } else if trimmed.starts_with('+') {
        COLOR_DIRECTION
    } else if trimmed.starts_with('-') {
        COLOR_DIRECTION
    } else if trimmed.starts_with('$') {
        COLOR_VARIABLE
    } else if trimmed.starts_with('?') || trimmed.starts_with('|') {
        COLOR_CHOICE
    } else {
        COLOR_DEFAULT
    }
}

/// Append a single line's colored segments to `job`.
///
/// Within a line, `"..."` string literals and trailing `--` comments are
/// always colored with their dedicated colors; everything else takes the
/// line's base color. Operates on `char`s (via `char_indices`) so multi-byte
/// UTF-8 content is preserved.
fn highlight_line(job: &mut egui::text::LayoutJob, line: &str) {
    let trimmed = line.trim_start();
    let leading_ws = &line[..line.len() - trimmed.len()];
    if !leading_ws.is_empty() {
        job.append(leading_ws, 0.0, text_format(COLOR_DEFAULT));
    }

    let base = line_base_color(trimmed);
    let chars: Vec<(usize, char)> = trimmed.char_indices().collect();
    let n = chars.len();

    let mut buf_start: Option<usize> = None;
    let mut buf_end: usize = 0;
    let mut i = 0;

    while i < n {
        let (bofs, c) = chars[i];

        if c == '"' {
            // Flush pending base-colored text, then consume a string literal.
            if let Some(s) = buf_start {
                job.append(&trimmed[s..buf_end], 0.0, text_format(base));
                buf_start = None;
            }
            let start = bofs;
            let mut end_byte = bofs + 1; // include the opening quote
            i += 1;
            while i < n {
                let (eb, cc) = chars[i];
                end_byte = eb + cc.len_utf8();
                i += 1;
                if cc == '"' {
                    break;
                }
            }
            job.append(&trimmed[start..end_byte], 0.0, text_format(COLOR_STRING));
            continue;
        }

        if c == '-' && i + 1 < n && chars[i + 1].1 == '-' {
            // `--` comment runs to the end of the line.
            if let Some(s) = buf_start {
                job.append(&trimmed[s..buf_end], 0.0, text_format(base));
            }
            job.append(&trimmed[bofs..], 0.0, text_format(COLOR_COMMENT));
            return;
        }

        // Accumulate into the base-colored run.
        if buf_start.is_none() {
            buf_start = Some(bofs);
        }
        buf_end = bofs + c.len_utf8();
        i += 1;
    }

    if let Some(s) = buf_start {
        job.append(&trimmed[s..buf_end], 0.0, text_format(base));
    }
}

/// Build a monospace `TextFormat` with the given text color.
fn text_format(color: egui::Color32) -> egui::text::TextFormat {
    egui::text::TextFormat {
        font_id: egui::FontId::monospace(FONT_SIZE),
        color,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_appends_extension() {
        assert_eq!(sanitize_filename("demo"), "demo.akrs");
        assert_eq!(sanitize_filename("demo.akrs"), "demo.akrs");
        assert_eq!(sanitize_filename("  a/b "), "ab.akrs");
        assert_eq!(sanitize_filename(""), "untitled.akrs");
    }

    #[test]
    fn highlight_preserves_text() {
        // The LayoutJob's `text` must equal the input so cursor positions
        // stay valid for the TextEdit.
        for src in [
            "",
            "hello",
            "# Title\nAki: \"Hi\"\n-- comment\n$x = 1\n",
            "多行\n中文 \"字\" 符\n",
        ] {
            let job = highlight_code(src);
            assert_eq!(job.text, src, "mismatch for {:?}", src);
        }
    }

    #[test]
    fn phase_and_position_labels() {
        assert_eq!(phase_label(EnginePhase::ChoicePending), "ChoicePending");
        assert_eq!(position_label(&Position::Left), "Left");
        assert_eq!(position_label(&Position::Custom(0.25)), "Custom(0.25)");
    }

    #[test]
    fn run_script_compiles_sample() {
        let mut app = EditorApp::default();
        app.run_script();
        assert!(app.engine.is_some(), "sample script should compile");
        assert!(
            app.diagnostics.iter().all(|d| !d.starts_with("[error]")),
            "no errors expected for the sample"
        );
    }
}
