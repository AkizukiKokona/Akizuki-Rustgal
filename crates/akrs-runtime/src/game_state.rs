//! Scene state: the render-ready snapshot of the current game scene.
//!
//! This is the "scene graph" — the renderer reads this structure each frame
//! to draw the current scene. It is updated by the engine as VM events arrive.
//!
//! No rendering code lives here; this is purely data. The renderer (e.g.,
//! `akrs_render` with macroquad) interprets this data to draw.

use akrs_core::{Position, SpriteTransform, Transition};
use serde::{Deserialize, Serialize};

/// Complete render state for one frame.
#[derive(Debug, Clone, Default)]
pub struct SceneState {
    /// Current background image (name/identifier).
    pub background: Option<BackgroundState>,
    /// Characters currently on stage.
    pub characters: Vec<CharacterState>,
    /// Active dialogue box (None if no dialogue).
    pub dialogue: Option<DialogueState>,
    /// Active choices (None if no choices displayed).
    pub choices: Option<ChoicesState>,
    /// Active transition overlay.
    pub transition: Option<TransitionOverlay>,
    /// Whether the title screen is showing.
    pub show_title: bool,
    /// Whether the story has ended.
    pub story_ended: bool,
    /// Current music track.
    pub music: Option<String>,
}

/// Background image state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackgroundState {
    /// Resource name/identifier.
    pub name: String,
    /// Alpha (0.0 - 1.0) for fade transitions.
    pub alpha: f32,
    /// Horizontal offset (-1.0 to 1.0) for slide transitions.
    pub offset_x: f32,
    /// Vertical offset (-1.0 to 1.0) for slide transitions.
    pub offset_y: f32,
}

impl Default for BackgroundState {
    fn default() -> Self {
        Self { name: String::new(), alpha: 1.0, offset_x: 0.0, offset_y: 0.0 }
    }
}

/// Character sprite on stage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterState {
    /// Character name (matches resource identifier).
    pub name: String,
    /// Pose/expression variant.
    pub pose: Option<String>,
    /// Screen position.
    pub position: Position,
    /// Alpha (0.0 - 1.0) for fade transitions.
    pub alpha: f32,
    /// Horizontal offset for slide transitions.
    pub offset_x: f32,
    /// Scale factor.
    pub scale: f32,
    /// 精确百分比位置（由 `at x,y` 语法设置）。
    /// None 时使用 position 字段。
    #[serde(default)]
    pub custom_x: Option<f32>,
    #[serde(default)]
    pub custom_y: Option<f32>,
}

impl Default for CharacterState {
    fn default() -> Self {
        Self {
            name: String::new(),
            pose: None,
            position: Position::Center,
            alpha: 1.0,
            offset_x: 0.0,
            scale: 1.0,
            custom_x: None,
            custom_y: None,
        }
    }
}

/// Dialogue box state with typewriter effect.
#[derive(Debug, Clone)]
pub struct DialogueState {
    pub speaker: String,
    pub pose: Option<String>,
    pub full_text: String,
    /// Number of characters currently displayed (typewriter).
    pub displayed_chars: usize,
    /// Whether the full text is displayed.
    pub complete: bool,
}

/// Choices state.
#[derive(Debug, Clone)]
pub struct ChoicesState {
    pub prompt: Option<String>,
    pub options: Vec<ChoiceOptionState>,
    /// Currently highlighted option (for keyboard navigation).
    pub selected: usize,
}

#[derive(Debug, Clone)]
pub struct ChoiceOptionState {
    pub text: String,
    pub available: bool,
}

/// Transition overlay state (read by renderer to apply visual effects).
#[derive(Debug, Clone)]
pub struct TransitionOverlay {
    pub kind: Transition,
    pub phase: TransitionPhase,
    /// 0.0 to 1.0.
    pub progress: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TransitionPhase {
    /// Old content fading out.
    Out,
    /// New content fading in.
    In,
}

impl SceneState {
    /// Create a new empty scene (title screen).
    pub fn new() -> Self {
        Self {
            show_title: true,
            ..Default::default()
        }
    }

    /// Clear dialogue and choices.
    pub fn clear_text(&mut self) {
        self.dialogue = None;
        self.choices = None;
    }

    /// Set dialogue text.
    pub fn set_dialogue(&mut self, speaker: String, pose: Option<String>, text: String) {
        self.dialogue = Some(DialogueState {
            speaker,
            pose,
            full_text: text,
            displayed_chars: 0,
            complete: false,
        });
    }

    /// Set narration (no speaker).
    pub fn set_narration(&mut self, text: String) {
        self.dialogue = Some(DialogueState {
            speaker: String::new(),
            pose: None,
            full_text: text,
            displayed_chars: 0,
            complete: false,
        });
    }

    /// Set choices.
    pub fn set_choices(&mut self, prompt: Option<String>, options: Vec<ChoiceOptionState>) {
        self.choices = Some(ChoicesState { prompt, options, selected: 0 });
    }

    /// Clear choices.
    pub fn clear_choices(&mut self) {
        self.choices = None;
    }

    /// Set background.
    pub fn set_background(&mut self, name: String) {
        self.background = Some(BackgroundState {
            name,
            alpha: 1.0,
            offset_x: 0.0,
            offset_y: 0.0,
        });
    }

    /// Add or update a character on stage.
    /// Position is auto-calculated based on the number of characters currently on stage:
    /// - 0 existing → new character gets Center
    /// - 1 existing → existing moves to Left, new gets Right
    /// Does not support more than 2 characters.
    pub fn character_enter(&mut self, name: String, pose: Option<String>) {
        self.character_enter_with(name, pose, SpriteTransform::default())
    }

    /// Add or update a character with a transform (位置百分比 + 大小倍数)。
    /// 如果 transform 中指定了 custom_x/custom_y，则使用精确百分比位置；
    /// 否则使用 auto_layout 自动布局。
    pub fn character_enter_with(
        &mut self,
        name: String,
        pose: Option<String>,
        transform: SpriteTransform,
    ) {
        // Remove existing instance of this character (re-enter replaces)
        self.characters.retain(|c| c.name != name);
        // 若提供了 size，则 scale 用之；否则默认 1.0
        let scale = transform.scale.unwrap_or(1.0);
        self.characters.push(CharacterState {
            name,
            pose,
            position: Position::Center, // temporary; auto_layout will fix if no custom_x
            alpha: 1.0,
            offset_x: 0.0,
            scale,
            custom_x: transform.x,
            custom_y: transform.y,
        });
        // 若有自定义位置，则不运行 auto_layout（保留自定义位置）
        if transform.x.is_none() {
            self.auto_layout();
        }
    }

    /// Add or update a character at an explicit position.
    /// Unlike `character_enter`, this does NOT run auto_layout — the given
    /// position is respected. Caller is responsible for the 2-character limit.
    pub fn character_enter_at(&mut self, name: String, pose: Option<String>, position: Position) {
        self.character_enter_at_with(name, pose, position, SpriteTransform::default())
    }

    /// 带 transform 的显式位置入场。
    pub fn character_enter_at_with(
        &mut self,
        name: String,
        pose: Option<String>,
        position: Position,
        transform: SpriteTransform,
    ) {
        self.characters.retain(|c| c.name != name);
        let scale = transform.scale.unwrap_or(1.0);
        self.characters.push(CharacterState {
            name,
            pose,
            position,
            alpha: 1.0,
            offset_x: 0.0,
            scale,
            custom_x: transform.x,
            custom_y: transform.y,
        });
    }

    /// Remove a character from stage and recalculate remaining positions.
    pub fn character_exit(&mut self, name: &str) {
        self.characters.retain(|c| c.name != name);
        self.auto_layout();
    }

    /// Automatically calculate character positions based on count.
    /// - 0 characters: no-op
    /// - 1 character: Center (50%)
    /// - 2 characters: first Left (25%), second Right (75%)
    pub fn auto_layout(&mut self) {
        match self.characters.len() {
            0 => {}
            1 => {
                self.characters[0].position = Position::Center;
            }
            _ => {
                self.characters[0].position = Position::Left;
                self.characters[1].position = Position::Right;
            }
        }
    }

    /// Check if a character is on stage.
    pub fn has_character(&self, name: &str) -> bool {
        self.characters.iter().any(|c| c.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_single_character_center() {
        let mut scene = SceneState::new();
        scene.character_enter("Aki".to_string(), None);
        assert_eq!(scene.characters.len(), 1);
        assert_eq!(scene.characters[0].position, Position::Center);
    }

    #[test]
    fn test_two_characters_left_right() {
        let mut scene = SceneState::new();
        scene.character_enter("Aki".to_string(), None);
        scene.character_enter("Yuki".to_string(), None);
        assert_eq!(scene.characters.len(), 2);
        assert_eq!(scene.characters[0].position, Position::Left);
        assert_eq!(scene.characters[1].position, Position::Right);
    }

    #[test]
    fn test_first_exits_second_returns_center() {
        let mut scene = SceneState::new();
        scene.character_enter("Aki".to_string(), None);
        scene.character_enter("Yuki".to_string(), None);
        // Aki (first, Left) exits
        scene.character_exit("Aki");
        assert_eq!(scene.characters.len(), 1);
        assert_eq!(scene.characters[0].name, "Yuki");
        assert_eq!(scene.characters[0].position, Position::Center);
    }

    #[test]
    fn test_all_exit_clears() {
        let mut scene = SceneState::new();
        scene.character_enter("Aki".to_string(), None);
        scene.character_enter("Yuki".to_string(), None);
        scene.character_exit("Aki");
        scene.character_exit("Yuki");
        assert_eq!(scene.characters.len(), 0);
        assert!(scene.characters.is_empty());
    }

    #[test]
    fn test_reenter_replaces_position() {
        let mut scene = SceneState::new();
        scene.character_enter("Aki".to_string(), None);
        scene.character_enter("Yuki".to_string(), None);
        // Re-enter Aki (should replace, not add a third)
        scene.character_enter("Aki".to_string(), Some("happy".to_string()));
        assert_eq!(scene.characters.len(), 2);
        assert_eq!(scene.characters[0].name, "Yuki");
        assert_eq!(scene.characters[0].position, Position::Left);
        assert_eq!(scene.characters[1].name, "Aki");
        assert_eq!(scene.characters[1].position, Position::Right);
    }

    #[test]
    fn test_enter_at_explicit_position() {
        let mut scene = SceneState::new();
        scene.character_enter_at("Aki".to_string(), None, Position::Left);
        assert_eq!(scene.characters.len(), 1);
        assert_eq!(scene.characters[0].position, Position::Left);
        // 手动位置不会被 auto_layout 覆盖
        scene.character_enter_at("Yuki".to_string(), None, Position::Right);
        assert_eq!(scene.characters.len(), 2);
        assert_eq!(scene.characters[0].position, Position::Left);
        assert_eq!(scene.characters[1].position, Position::Right);
    }
}
