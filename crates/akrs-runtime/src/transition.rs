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
use akrs_core::Transition;

/// Pending scene change to apply mid-transition.
struct PendingChange {
    new_background: Option<Option<String>>, // Outer: change bg; Inner: None = clear
    characters_enter: Vec<(String, Option<String>)>,
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
        chars_enter: Vec<(String, Option<String>)>,
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
        chars_enter: &[(String, Option<String>)],
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

        for (name, pose) in chars_enter {
            scene.character_enter(name.clone(), pose.clone());
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
fn ease_in(t: f32) -> f32 {
    t * t
}

/// Easing for fade-in (ease-out: fast start, slow end).
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
}
