//! Transition animation manager.
//!
//! Uses a two-phase state machine (Out → Swap → In) to produce smooth
//! transitions between scene states. The manager tracks timing; the renderer
//! applies visual effects based on the phase and progress values.
//!
//! # Transition Types
//!
//! - **Fade / FadeBlack / FadeWhite**: Fade to a color, swap, fade from color.
//! - **Dissolve**: Crossfade between old and new content.
//! - **Slide***: Slide old content out, slide new content in.
//! - **Wipe***: Wipe old content away revealing new content.
//! - **Blur**: Blur old content, swap, unblur new content.
//! - **Instant**: Immediate cut (no animation).
//!
//! All transitions use easing for smooth motion (no linear interpolation
//! except for instant cuts), targeting 60fps with zero per-frame allocations.

use crate::game_state::{SceneState, TransitionOverlay, TransitionPhase};
use akrs_core::{Position, SpriteTransform, Transition};

/// Pending scene change to apply mid-transition.
struct PendingChange {
    new_background: Option<Option<String>>, // Outer: change bg; Inner: None = clear
    characters_enter: Vec<(String, Option<String>, Option<Position>, SpriteTransform)>,
    characters_exit: Vec<String>,
    music: Option<Option<String>>,
}

/// Manages transition animations.
pub struct TransitionManager {
    /// Current phase (Idle when no transition).
    phase: TransitionPhase,
    /// Whether a transition is active.
    active: bool,
    /// Transition kind.
    kind: Transition,
    /// Progress 0.0 to 1.0 for current phase.
    progress: f32,
    /// Duration of each phase (half of total) in seconds.
    half_duration: f32,
    /// Pending scene changes to apply at the swap point.
    pending: Option<PendingChange>,
}

impl Default for TransitionManager {
    fn default() -> Self {
        Self {
            phase: TransitionPhase::Out,
            active: false,
            kind: Transition::Instant,
            progress: 0.0,
            half_duration: 0.3,
            pending: None,
        }
    }
}

impl TransitionManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a transition with pending scene changes.
    pub fn start(
        &mut self,
        kind: Transition,
        scene: &mut SceneState,
        new_bg: Option<Option<String>>,
        chars_enter: Vec<(String, Option<String>, Option<Position>, SpriteTransform)>,
        chars_exit: Vec<String>,
        new_music: Option<Option<String>>,
    ) {
        if kind == Transition::Instant {
            // Instant: apply changes immediately, no animation
            Self::apply_changes(scene, new_bg, &chars_enter, &chars_exit, new_music);
            self.active = false;
            return;
        }

        self.kind = kind;
        self.phase = TransitionPhase::Out;
        self.progress = 0.0;
        self.half_duration = kind.default_duration() / 2.0;
        self.pending = Some(PendingChange {
            new_background: new_bg,
            characters_enter: chars_enter,
            characters_exit: chars_exit,
            music: new_music,
        });
        self.active = true;

        // Set transition overlay on scene
        scene.transition = Some(TransitionOverlay {
            kind,
            phase: TransitionPhase::Out,
            progress: 0.0,
        });
    }

    /// Check if a transition is currently active.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// 是否有待应用的场景变更（即处于 Out 阶段、尚未到 swap point）。
    /// 存档时据此决定是否需要把 pending 应用到快照上。
    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// 把当前 pending 变更应用到给定场景，不修改过渡管理器自身状态。
    ///
    /// 用途：存档构造快照时，让快照与 VM 指令指针保持一致——
    /// Out 阶段中现场 `scene` 仍是过渡前的旧状态，但 VM 已经越过触发
    /// 过渡的指令（入场/下场/换背景）。若把旧状态原样存入快照，读档后
    /// 这些 pending 变更永远不会被执行（VM 不会再重放那条指令），于是
    /// 本该下场的立绘就"下不去"了。把 pending 应用到快照即可避免。
    pub fn apply_pending_to(&self, scene: &mut SceneState) {
        if let Some(pending) = &self.pending {
            Self::apply_changes(
                scene,
                pending.new_background.clone(),
                &pending.characters_enter,
                &pending.characters_exit,
                pending.music.clone(),
            );
        }
    }

    /// 立即清空过渡状态（active/pending/进度全部归零）。
    ///
    /// 读档时调用：存档不保存过渡状态，读档后从 VM 指针继续执行，
    /// 任何残留的过渡状态（尤其来自读档前的现场）都是过时的，必须清空，
    /// 否则残留的 pending 会在下一帧 swap point 被错误地应用到刚恢复的场景。
    /// 注意：此方法不动 `scene.transition` 叠层，调用方需自行清空。
    pub fn reset(&mut self) {
        self.active = false;
        self.pending = None;
        self.progress = 0.0;
        self.phase = TransitionPhase::Out;
    }

    /// Update the transition. Returns true if the transition completed this frame.
    pub fn update(&mut self, dt: f32, scene: &mut SceneState) -> bool {
        if !self.active {
            return false;
        }

        // Avoid division by zero
        if self.half_duration <= 0.0 {
            self.progress = 1.0;
        } else {
            self.progress += dt / self.half_duration;
        }

        if self.progress >= 1.0 {
            self.progress = 1.0;
            match self.phase {
                TransitionPhase::Out => {
                    // Swap point: apply pending changes
                    if let Some(pending) = self.pending.take() {
                        Self::apply_changes(
                            scene,
                            pending.new_background,
                            &pending.characters_enter,
                            &pending.characters_exit,
                            pending.music,
                        );
                    }
                    // Switch to "In" phase
                    self.phase = TransitionPhase::In;
                    self.progress = 0.0;
                    if let Some(overlay) = &mut scene.transition {
                        overlay.phase = TransitionPhase::In;
                        overlay.progress = 0.0;
                    }
                    false
                }
                TransitionPhase::In => {
                    // Transition complete
                    self.active = false;
                    scene.transition = None;
                    true
                }
            }
        } else {
            // Update overlay progress with easing
            if let Some(overlay) = &mut scene.transition {
                overlay.phase = self.phase;
                overlay.progress = ease_in_out(self.progress);
            }
            false
        }
    }

    /// Apply pending scene changes directly.
    fn apply_changes(
        scene: &mut SceneState,
        new_bg: Option<Option<String>>,
        chars_enter: &[(String, Option<String>, Option<Position>, SpriteTransform)],
        chars_exit: &[String],
        new_music: Option<Option<String>>,
    ) {
        if let Some(bg) = new_bg {
            match bg {
                Some(name) => scene.set_background(name),
                None => scene.background = None,
            }
        }

        for name in chars_exit {
            scene.character_exit(name);
        }

        for (name, pose, position, transform) in chars_enter {
            match position {
                Some(pos) => scene.character_enter_at_with(name.clone(), pose.clone(), *pos, *transform),
                None => scene.character_enter_with(name.clone(), pose.clone(), *transform),
            }
        }

        if let Some(music) = new_music {
            scene.music = music;
        }
    }
}

/// Smooth ease-in-out curve (cubic).
fn ease_in_out(t: f32) -> f32 {
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        let f = 2.0 * t - 2.0;
        1.0 + f * f * f / 2.0
    }
}

/// Easing for fade-out (ease-in: slow start).
#[allow(dead_code)]
fn ease_in(t: f32) -> f32 {
    t * t
}

/// Easing for fade-in (ease-out: fast start, slow end).
#[allow(dead_code)]
fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t) * (1.0 - t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_instant_transition() {
        let mut tm = TransitionManager::new();
        let mut scene = SceneState::new();
        tm.start(
            Transition::Instant,
            &mut scene,
            Some(Some("bg1".to_string())),
            vec![],
            vec![],
            None,
        );
        assert!(!tm.is_active());
        assert!(scene.background.is_some());
        assert_eq!(scene.background.as_ref().unwrap().name, "bg1");
    }

    #[test]
    fn test_fade_transition_phases() {
        let mut tm = TransitionManager::new();
        let mut scene = SceneState::new();

        // Start a fade transition (0.6s total, 0.3s per phase)
        tm.start(
            Transition::Fade,
            &mut scene,
            Some(Some("new_bg".to_string())),
            vec![],
            vec![],
            None,
        );
        assert!(tm.is_active());
        assert!(scene.transition.is_some());

        // Update through "Out" phase (0.3s)
        let done = tm.update(0.3, &mut scene);
        assert!(!done); // Not done yet, just switched to "In"
        assert!(tm.is_active());
        // Background should have been swapped
        assert_eq!(scene.background.as_ref().unwrap().name, "new_bg");

        // Update through "In" phase (0.3s)
        let done = tm.update(0.3, &mut scene);
        assert!(done); // Transition complete
        assert!(!tm.is_active());
        assert!(scene.transition.is_none());
    }

    #[test]
    fn test_ease_in_out() {
        assert!((ease_in_out(0.0) - 0.0).abs() < 0.001);
        assert!((ease_in_out(0.5) - 0.5).abs() < 0.001);
        assert!((ease_in_out(1.0) - 1.0).abs() < 0.001);
    }

    /// 验证：过渡 Out 阶段中 pending 的角色下场，应用到一个场景副本后
    /// 该角色应被移除——这正是存档快照构造时所需要的（让快照与 VM 指针
    /// 一致，避免读档后立绘"下不去"）。
    #[test]
    fn test_apply_pending_to_snapshot_removes_exiting_character() {
        let mut tm = TransitionManager::new();
        let mut scene = SceneState::new();
        scene.character_enter("Aki".to_string(), None);
        scene.character_enter("Yuki".to_string(), None);
        assert_eq!(scene.characters.len(), 2);

        // 启动一个 Fade 过渡，pending 中 Yuki 待下场。
        tm.start(
            Transition::Fade,
            &mut scene,
            None,
            vec![],
            vec!["Yuki".to_string()],
            None,
        );
        assert!(tm.has_pending());
        // 现场 scene 仍含 Yuki（pending 尚未到 swap point）。
        assert!(scene.has_character("Yuki"));

        // 构造快照副本并应用 pending：Yuki 应被移除。
        let mut snap = scene.clone();
        tm.apply_pending_to(&mut snap);
        assert!(!snap.has_character("Yuki"));
        assert_eq!(snap.characters.len(), 1);
        assert_eq!(snap.characters[0].name, "Aki");

        // 关键：apply_pending_to 不得修改现场 scene 或过渡管理器状态。
        assert!(scene.has_character("Yuki"));
        assert!(tm.is_active());
        assert!(tm.has_pending());
    }

    /// 验证：apply_pending_to 在没有 pending 时是 no-op。
    #[test]
    fn test_apply_pending_to_noop_without_pending() {
        let tm = TransitionManager::new();
        let mut scene = SceneState::new();
        scene.character_enter("Aki".to_string(), None);
        let before_count = scene.characters.len();
        let before_name = scene.characters[0].name.clone();
        tm.apply_pending_to(&mut scene);
        assert_eq!(scene.characters.len(), before_count);
        assert_eq!(scene.characters[0].name, before_name);
    }

    /// 验证：reset() 清空所有过渡状态，残留 pending 不会再被应用。
    #[test]
    fn test_reset_clears_pending_and_active() {
        let mut tm = TransitionManager::new();
        let mut scene = SceneState::new();
        scene.character_enter("Aki".to_string(), None);
        tm.start(
            Transition::Fade,
            &mut scene,
            None,
            vec![],
            vec!["Aki".to_string()],
            None,
        );
        assert!(tm.is_active());
        assert!(tm.has_pending());

        tm.reset();
        assert!(!tm.is_active());
        assert!(!tm.has_pending());

        // reset 后 update 不应再改动场景（Aki 仍在场，pending 下场被丢弃）。
        let _ = tm.update(1.0, &mut scene);
        assert!(scene.has_character("Aki"));
    }
}
