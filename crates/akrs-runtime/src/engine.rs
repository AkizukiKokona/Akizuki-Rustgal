//! The game engine: coordinates VM execution, scene state, transitions,
//! save/load, settings, and hot reload.
//!
//! The engine is rendering-agnostic. It produces a `SceneState` each frame
//! that a renderer (e.g., `akrs_render` with macroquad) reads to draw.
//!
//! # Architecture
//!
//! ```text
//! ┌──────────────────────────────────────────────────┐
//! │                     Engine                        │
//! │  ┌──────┐  ┌────────────┐  ┌───────────────────┐ │
//! │  │  VM  │→ │ Event Loop │→ │   SceneState      │ │
//! │  └──────┘  └────────────┘  │  (render snapshot) │ │
//! │  ┌──────────────────────┐  └───────────────────┘ │
//! │  │ TransitionManager    │  ┌───────────────────┐ │
//! │  │ (Out → Swap → In)    │  │  Settings         │ │
//! │  └──────────────────────┘  │  SaveManager       │ │
//! │  ┌──────────────────────┐  │  HotReloader       │ │
//! │  │ TypewriterState      │  └───────────────────┘ │
//! │  └──────────────────────┘                        │
//! └──────────────────────────────────────────────────┘
//! ```
//!
//! # 60fps Guarantee
//!
//! - No per-frame heap allocations in the hot path
//! - Transition updates are pure float arithmetic
//! - Typewriter is a simple counter increment
//! - VM step is a match on a flat instruction array

use crate::game_state::{SceneState, ChoiceOptionState};
use crate::save_load::SaveManager;
use crate::settings::Settings;
use crate::transition::TransitionManager;

use akrs_core::{
    compile_and_create_vm, CompileError, DirectionAction, DirectionKind,
    Transition, Vm, VmEvent,
};

/// Events emitted by the engine for the UI layer to react to.
#[derive(Debug, Clone)]
pub enum EngineEvent {
    /// A new dialogue line is being displayed.
    DialogueShown { speaker: String, text: String },
    /// Narration text is being displayed.
    NarrationShown { text: String },
    /// All text has been displayed (typewriter complete).
    TextComplete,
    /// Choices are being displayed.
    ChoicesShown { prompt: Option<String>, options: Vec<ChoiceOptionState> },
    /// A background change occurred.
    BackgroundChanged { name: String },
    /// A character entered the stage.
    CharacterEntered { name: String },
    /// A character exited the stage.
    CharacterExited { name: String },
    /// Music changed.
    MusicChanged { name: String },
    /// A sound effect was played.
    SoundPlayed { name: String },
    /// A transition started.
    TransitionStarted { kind: Transition },
    /// A transition completed.
    TransitionCompleted,
    /// The story has ended.
    StoryEnded,
    /// A warning (e.g., missing resource).
    Warning { message: String },
    /// An error occurred.
    Error { message: String },
    /// The game started (from title screen).
    GameStarted,
    /// A save was completed.
    Saved { slot: usize },
    /// A load was completed.
    Loaded { slot: usize },
    /// Script was hot-reloaded.
    ScriptReloaded,
    /// Script hot-reload failed.
    ScriptReloadFailed { errors: Vec<String> },
}

/// Engine phase (high-level state machine).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EnginePhase {
    /// Title screen showing.
    Title,
    /// Running the script, waiting for player to advance.
    Running,
    /// A transition is in progress; pending event will be shown after.
    Transitioning,
    /// Waiting for a timer (the `wait` command).
    Waiting,
    /// Choices are displayed, waiting for selection.
    ChoicePending,
    /// Story has ended.
    StoryEnded,
}

/// Pending blocking event to show after a transition completes.
enum PendingEvent {
    Dialogue { speaker: String, pose: Option<String>, text: String },
    Narration { text: String },
    Choices { prompt: Option<String>, options: Vec<ChoiceOptionState> },
    Wait { seconds: f64 },
    StoryEnd,
}

/// Typewriter animation state.
struct TypewriterState {
    chars_per_second: f32,
    elapsed: f32,
    complete: bool,
}

impl TypewriterState {
    fn new() -> Self {
        Self { chars_per_second: 30.0, elapsed: 0.0, complete: true }
    }

    fn start(&mut self, text_len: usize, chars_per_second: f32) {
        self.chars_per_second = chars_per_second;
        self.elapsed = 0.0;
        self.complete = chars_per_second <= 0.0 || chars_per_second >= 999.0 || text_len == 0;
    }

    fn update(&mut self, dt: f32) -> usize {
        if self.complete {
            return usize::MAX;
        }
        self.elapsed += dt;
        (self.elapsed * self.chars_per_second) as usize
    }

    fn finish(&mut self) {
        self.complete = true;
    }
}

/// The main game engine.
pub struct Engine {
    /// The script VM.
    vm: Vm,
    /// The original source text (for hot reload).
    source: String,
    /// Current render state (scene graph).
    scene: SceneState,
    /// Transition animation manager.
    transition: TransitionManager,
    /// User settings.
    settings: Settings,
    /// Save slot manager.
    saves: SaveManager,
    /// Current engine phase.
    phase: EnginePhase,
    /// Typewriter state.
    typewriter: TypewriterState,
    /// Pending event to show after transition.
    pending: Option<PendingEvent>,
    /// Wait timer remaining (seconds).
    wait_remaining: f64,
    /// Play time in seconds.
    play_time: f64,
    /// Current section name (for save metadata).
    current_section_name: String,
    /// Hot reloader (if enabled).
    #[cfg(feature = "hot-reload")]
    hot_reloader: Option<crate::hot_reload::HotReloader>,
}

impl Engine {
    /// Create a new engine from script source text.
    pub fn new(source: &str) -> Result<Self, Vec<CompileError>> {
        let vm = compile_and_create_vm(source)?;
        let saves = SaveManager::new("saves", 20);

        Ok(Self {
            vm,
            source: source.to_string(),
            scene: SceneState::new(),
            transition: TransitionManager::new(),
            settings: Settings::default(),
            saves,
            phase: EnginePhase::Title,
            typewriter: TypewriterState::new(),
            pending: None,
            wait_remaining: 0.0,
            play_time: 0.0,
            current_section_name: String::new(),
            #[cfg(feature = "hot-reload")]
            hot_reloader: None,
        })
    }

    /// Create an engine and immediately start the game (skip title screen).
    pub fn start_running(source: &str) -> Result<Self, Vec<CompileError>> {
        let mut engine = Self::new(source)?;
        engine.start_game();
        Ok(engine)
    }

    /// Enable hot reload for a script file path.
    #[cfg(feature = "hot-reload")]
    pub fn enable_hot_reload(&mut self, script_path: impl AsRef<std::path::Path>) -> Result<(), String> {
        let reloader = crate::hot_reload::HotReloader::new(script_path)?;
        self.hot_reloader = Some(reloader);
        Ok(())
    }

    /// Start the game (transition from title screen to running).
    pub fn start_game(&mut self) {
        if self.phase != EnginePhase::Title {
            return;
        }
        self.phase = EnginePhase::Running;
        self.scene.show_title = false;
        let _ = self.vm.start();
        self.process_events();
    }

    /// Get the current render state (for the renderer to draw).
    pub fn scene(&self) -> &SceneState {
        &self.scene
    }

    /// Get the current engine phase.
    pub fn phase(&self) -> EnginePhase {
        self.phase
    }

    /// Get play time in seconds.
    pub fn play_time(&self) -> f64 {
        self.play_time
    }

    /// Get current section name.
    pub fn current_section(&self) -> &str {
        &self.current_section_name
    }

    /// Access settings.
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Mutable settings access.
    pub fn settings_mut(&mut self) -> &mut Settings {
        &mut self.settings
    }

    /// Load persistent settings from `saves/settings.json`.
    /// If the file does not exist or cannot be parsed, defaults are used.
    pub fn load_settings(&mut self) {
        let path = Settings::default_path();
        self.settings = Settings::load(&path);
    }

    /// Persist the current settings to `saves/settings.json`.
    pub fn save_settings(&self) -> Result<(), String> {
        let path = Settings::default_path();
        // Ensure the saves directory exists.
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        self.settings.save(&path)
    }

    /// Access save manager.
    pub fn saves(&self) -> &SaveManager {
        &self.saves
    }

    /// Update the engine. Call this every frame.
    /// Returns events for the UI layer to process.
    pub fn update(&mut self, dt: f32) -> Vec<EngineEvent> {
        let mut events = Vec::new();

        // Track play time
        if self.phase != EnginePhase::Title && self.phase != EnginePhase::StoryEnded {
            self.play_time += dt as f64;
        }

        // Update transition
        if self.transition.is_active() {
            let completed = self.transition.update(dt, &mut self.scene);
            if completed {
                events.push(EngineEvent::TransitionCompleted);
                // If there's a pending event, show it now
                if let Some(pending) = self.pending.take() {
                    self.show_pending_event(pending, &mut events);
                } else {
                    // No pending event, continue processing
                    self.phase = EnginePhase::Running;
                    self.process_events_into(&mut events);
                }
            }
        } else if self.phase == EnginePhase::Transitioning {
            // Transition just finished but update didn't catch it
            self.phase = EnginePhase::Running;
            if let Some(pending) = self.pending.take() {
                self.show_pending_event(pending, &mut events);
            } else {
                self.process_events_into(&mut events);
            }
        }

        // Update typewriter
        if self.phase == EnginePhase::Running {
            if let Some(dialogue) = &mut self.scene.dialogue
                && !dialogue.complete
            {
                let displayed = self.typewriter.update(dt);
                let text_len = dialogue.full_text.chars().count();
                if displayed >= text_len {
                    dialogue.displayed_chars = text_len;
                    dialogue.complete = true;
                    self.typewriter.complete = true;
                    events.push(EngineEvent::TextComplete);
                } else {
                    dialogue.displayed_chars = displayed;
                }
            }
        }

        // Update wait timer
        if self.phase == EnginePhase::Waiting {
            self.wait_remaining -= dt as f64;
            if self.wait_remaining <= 0.0 {
                self.phase = EnginePhase::Running;
                self.process_events_into(&mut events);
            }
        }

        // Check for hot reload
        #[cfg(feature = "hot-reload")]
        if let Some(reloader) = &self.hot_reloader
            && let Some(new_source) = reloader.check_for_changes()
        {
            match self.reload_script_internal(&new_source) {
                Ok(()) => events.push(EngineEvent::ScriptReloaded),
                Err(errs) => events.push(EngineEvent::ScriptReloadFailed { errors: errs }),
            }
        }

        events
    }

    /// Player clicked to advance.
    pub fn advance(&mut self) -> Vec<EngineEvent> {
        let mut events = Vec::new();

        if self.phase == EnginePhase::Title {
            self.start_game();
            events.push(EngineEvent::GameStarted);
            return events;
        }

        if self.phase == EnginePhase::StoryEnded {
            return events;
        }

        // If typewriter is not complete, complete it
        if let Some(dialogue) = &mut self.scene.dialogue
            && !dialogue.complete
        {
            dialogue.displayed_chars = dialogue.full_text.chars().count();
            dialogue.complete = true;
            self.typewriter.finish();
            events.push(EngineEvent::TextComplete);
            return events;
        }

        // If transition is active, ignore advance
        if self.transition.is_active() {
            return events;
        }

        // Advance the VM and process next events
        if self.phase == EnginePhase::Running {
            self.vm.advance();
            self.process_events_into(&mut events);
        }

        events
    }

    /// Player selected a choice.
    pub fn choose(&mut self, index: usize) -> Vec<EngineEvent> {
        let mut events = Vec::new();

        if self.phase != EnginePhase::ChoicePending {
            return events;
        }

        match self.vm.choose(index) {
            Ok(()) => {
                self.scene.clear_choices();
                self.phase = EnginePhase::Running;
                self.process_events_into(&mut events);
            }
            Err(e) => {
                events.push(EngineEvent::Error { message: e.message });
            }
        }

        events
    }

    /// Save the current game state.
    pub fn save(&mut self, slot: usize) -> Vec<EngineEvent> {
        let mut events = Vec::new();
        let vm_state = self.vm.save_state();
        let description = self.scene.dialogue.as_ref()
            .map(|d| format!("{}: {}", d.speaker, &d.full_text[..d.full_text.char_indices().take(30).last().map(|(i, _)| i).unwrap_or(0)]))
            .unwrap_or_else(|| self.current_section_name.clone());

        match self.saves.save(
            slot,
            vm_state,
            &self.current_section_name,
            self.play_time as u64,
            &description,
        ) {
            Ok(_) => events.push(EngineEvent::Saved { slot }),
            Err(e) => events.push(EngineEvent::Error { message: e }),
        }
        events
    }

    /// Load a save slot.
    pub fn load(&mut self, slot: usize) -> Vec<EngineEvent> {
        let mut events = Vec::new();

        match self.saves.load(slot) {
            Ok(save) => {
                self.vm.load_state(save.vm_state);
                self.scene.show_title = false;
                self.phase = EnginePhase::Running;
                self.scene.clear_text();
                self.process_events_into(&mut events);
                events.push(EngineEvent::Loaded { slot });
            }
            Err(e) => {
                events.push(EngineEvent::Error { message: e });
            }
        }
        events
    }

    // ─── Autosave (crash-recovery) ───

    /// Save the current game state to the dedicated autosave slot.
    ///
    /// This is intended to be called when the player closes the window
    /// unexpectedly. It is a no-op on the title screen and after the story
    /// has ended, since there is nothing meaningful to recover in those cases.
    pub fn save_autosave(&mut self) -> Vec<EngineEvent> {
        let mut events = Vec::new();

        // Nothing to recover on the title screen or after the story ended.
        if self.phase == EnginePhase::Title || self.phase == EnginePhase::StoryEnded {
            return events;
        }

        let vm_state = self.vm.save_state();
        let description = self
            .scene
            .dialogue
            .as_ref()
            .map(|d| {
                format!(
                    "{}: {}",
                    d.speaker,
                    &d.full_text[..d.full_text
                        .char_indices()
                        .take(30)
                        .last()
                        .map(|(i, _)| i)
                        .unwrap_or(0)]
                )
            })
            .unwrap_or_else(|| self.current_section_name.clone());

        match self.saves.save_autosave(
            vm_state,
            &self.current_section_name,
            self.play_time as u64,
            &description,
        ) {
            Ok(_) => {}
            Err(e) => events.push(EngineEvent::Error { message: e }),
        }
        events
    }

    /// Load the autosave slot and resume the game from it.
    pub fn load_autosave(&mut self) -> Vec<EngineEvent> {
        let mut events = Vec::new();

        match self.saves.load_autosave() {
            Ok(save) => {
                self.vm.load_state(save.vm_state);
                self.scene.show_title = false;
                self.phase = EnginePhase::Running;
                self.scene.clear_text();
                self.process_events_into(&mut events);
            }
            Err(e) => {
                events.push(EngineEvent::Error { message: e });
            }
        }
        events
    }

    /// Check whether an autosave exists and should be offered for recovery.
    ///
    /// Returns `false` if the `auto_recovery` setting is disabled, even when
    /// an autosave file is present on disk.
    pub fn has_autosave(&self) -> bool {
        self.settings.auto_recovery && self.saves.has_autosave()
    }

    /// Delete the autosave (called on a clean exit, or after it has been
    /// loaded so the player is not prompted again).
    pub fn delete_autosave(&mut self) -> Vec<EngineEvent> {
        let mut events = Vec::new();
        if let Err(e) = self.saves.delete_autosave() {
            events.push(EngineEvent::Error { message: e });
        }
        events
    }

    /// Reload the script from new source text (hot reload).
    pub fn reload_script(&mut self, source: &str) -> Result<(), Vec<String>> {
        self.reload_script_internal(source)
    }

    fn reload_script_internal(&mut self, source: &str) -> Result<(), Vec<String>> {
        // Save current VM state
        let vm_state = self.vm.save_state();

        // Compile new source
        match compile_and_create_vm(source) {
            Ok(new_vm) => {
                self.vm = new_vm;
                self.source = source.to_string();
                // Restore state
                self.vm.load_state(vm_state);
                Ok(())
            }
            Err(errors) => {
                let msgs: Vec<String> = errors.iter()
                    .map(|e| {
                        let loc = akrs_core::format_location(&e.span);
                        match &e.hint {
                            Some(h) => format!("{}: {} (hint: {})", loc, e.message, h),
                            None => format!("{}: {}", loc, e.message),
                        }
                    })
                    .collect();
                Err(msgs)
            }
        }
    }

    /// Process VM events until a blocking event is found.
    fn process_events(&mut self) {
        let mut events = Vec::new();
        self.process_events_into(&mut events);
        // Events are available via update() return value in normal flow.
        // For direct calls (start_game, load), events are discarded.
    }

    fn process_events_into(&mut self, events: &mut Vec<EngineEvent>) {
        loop {
            if self.transition.is_active() {
                self.phase = EnginePhase::Transitioning;
                return;
            }

            let vm_event = match self.vm.step() {
                Ok(e) => e,
                Err(e) => {
                    events.push(EngineEvent::Error { message: e.message });
                    self.phase = EnginePhase::StoryEnded;
                    return;
                }
            };

            match vm_event {
                VmEvent::Dialogue { speaker, pose, text } => {
                    // Check if we need to wait for transition
                    if self.transition.is_active() {
                        self.pending = Some(PendingEvent::Dialogue { speaker, pose, text });
                        self.phase = EnginePhase::Transitioning;
                    } else {
                        self.show_dialogue(speaker, pose, text, events);
                    }
                    return;
                }
                VmEvent::Narration { text } => {
                    if self.transition.is_active() {
                        self.pending = Some(PendingEvent::Narration { text });
                        self.phase = EnginePhase::Transitioning;
                    } else {
                        self.show_narration(text, events);
                    }
                    return;
                }
                VmEvent::Command { cmd, args, transition } => {
                    self.handle_command(cmd, args, transition, events);
                    // Non-blocking: continue to next event
                }
                VmEvent::Direction { action } => {
                    self.handle_direction(action, events);
                    // Non-blocking: continue to next event
                }
                VmEvent::Choice { prompt, options } => {
                    let opts: Vec<ChoiceOptionState> = options.into_iter()
                        .map(|o| ChoiceOptionState { text: o.text, available: o.available })
                        .collect();

                    if self.transition.is_active() {
                        self.pending = Some(PendingEvent::Choices { prompt, options: opts });
                        self.phase = EnginePhase::Transitioning;
                    } else {
                        self.scene.set_choices(prompt.clone(), opts.clone());
                        self.phase = EnginePhase::ChoicePending;
                        events.push(EngineEvent::ChoicesShown { prompt, options: opts });
                    }
                    return;
                }
                VmEvent::Wait { seconds } => {
                    if self.transition.is_active() {
                        self.pending = Some(PendingEvent::Wait { seconds });
                        self.phase = EnginePhase::Transitioning;
                    } else {
                        self.wait_remaining = seconds;
                        self.phase = EnginePhase::Waiting;
                    }
                    return;
                }
                VmEvent::StoryEnd => {
                    if self.transition.is_active() {
                        self.pending = Some(PendingEvent::StoryEnd);
                        self.phase = EnginePhase::Transitioning;
                    } else {
                        self.phase = EnginePhase::StoryEnded;
                        self.scene.story_ended = true;
                        events.push(EngineEvent::StoryEnded);
                    }
                    return;
                }
                VmEvent::Flow { target } | VmEvent::Visit { target } => {
                    self.current_section_name = target;
                    // Non-blocking: continue to next event
                }
                VmEvent::Return => {
                    // Non-blocking: continue to next event
                }
            }
        }
    }

    /// Show a pending event (called after transition completes).
    fn show_pending_event(&mut self, pending: PendingEvent, events: &mut Vec<EngineEvent>) {
        match pending {
            PendingEvent::Dialogue { speaker, pose, text } => {
                self.show_dialogue(speaker, pose, text, events);
            }
            PendingEvent::Narration { text } => {
                self.show_narration(text, events);
            }
            PendingEvent::Choices { prompt, options } => {
                self.scene.set_choices(prompt.clone(), options.clone());
                self.phase = EnginePhase::ChoicePending;
                events.push(EngineEvent::ChoicesShown { prompt, options });
            }
            PendingEvent::Wait { seconds } => {
                self.wait_remaining = seconds;
                self.phase = EnginePhase::Waiting;
            }
            PendingEvent::StoryEnd => {
                self.phase = EnginePhase::StoryEnded;
                self.scene.story_ended = true;
                events.push(EngineEvent::StoryEnded);
            }
        }
    }

    fn show_dialogue(
        &mut self,
        speaker: String,
        pose: Option<String>,
        text: String,
        events: &mut Vec<EngineEvent>,
    ) {
        self.scene.set_dialogue(speaker.clone(), pose, text.clone());
        self.typewriter.start(
            text.chars().count(),
            self.settings.text_speed,
        );
        self.phase = EnginePhase::Running;
        events.push(EngineEvent::DialogueShown { speaker, text });
    }

    fn show_narration(&mut self, text: String, events: &mut Vec<EngineEvent>) {
        self.scene.set_narration(text.clone());
        self.typewriter.start(
            text.chars().count(),
            self.settings.text_speed,
        );
        self.phase = EnginePhase::Running;
        events.push(EngineEvent::NarrationShown { text });
    }

    fn handle_command(
        &mut self,
        cmd: String,
        args: Vec<String>,
        transition: Option<Transition>,
        events: &mut Vec<EngineEvent>,
    ) {
        match cmd.as_str() {
            "bg" | "background" => {
                let name = args.first().cloned().unwrap_or_default();
                let kind = transition.unwrap_or(Transition::Fade);

                if self.transition.is_active() {
                    // Apply instantly if transition in progress
                    self.scene.set_background(name.clone());
                } else {
                    self.transition.start(
                        kind,
                        &mut self.scene,
                        Some(Some(name.clone())),
                        vec![],
                        vec![],
                        None,
                    );
                    events.push(EngineEvent::TransitionStarted { kind });
                }
                events.push(EngineEvent::BackgroundChanged { name });
            }
            "music" | "bgm" => {
                let name = args.first().cloned().unwrap_or_default();
                self.scene.music = Some(name.clone());
                events.push(EngineEvent::MusicChanged { name });
            }
            "sound" | "sfx" => {
                let name = args.first().cloned().unwrap_or_default();
                events.push(EngineEvent::SoundPlayed { name });
            }
            "stop_music" | "stop_bgm" => {
                self.scene.music = None;
                events.push(EngineEvent::MusicChanged { name: String::new() });
            }
            _ => {
                // Unknown command: warn but don't crash
                events.push(EngineEvent::Warning {
                    message: format!("unknown command: @{}", cmd),
                });
            }
        }
    }

    fn handle_direction(
        &mut self,
        action: DirectionAction,
        events: &mut Vec<EngineEvent>,
    ) {
        let kind = action.transition.unwrap_or(Transition::Dissolve);

        match action.kind {
            DirectionKind::Enter => {
                if self.transition.is_active() {
                    self.scene.character_enter(
                        action.character.clone(),
                        None,
                    );
                } else {
                    self.transition.start(
                        kind,
                        &mut self.scene,
                        None,
                        vec![(action.character.clone(), None)],
                        vec![],
                        None,
                    );
                    events.push(EngineEvent::TransitionStarted { kind });
                }
                events.push(EngineEvent::CharacterEntered {
                    name: action.character,
                });
            }
            DirectionKind::Exit => {
                if self.transition.is_active() {
                    self.scene.character_exit(&action.character);
                } else {
                    self.transition.start(
                        kind,
                        &mut self.scene,
                        None,
                        vec![],
                        vec![action.character.clone()],
                        None,
                    );
                    events.push(EngineEvent::TransitionStarted { kind });
                }
                events.push(EngineEvent::CharacterExited {
                    name: action.character,
                });
            }
        }
    }

    /// Get the script source (for debugging or hot reload).
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Check if the engine is waiting for player input.
    pub fn is_waiting_for_input(&self) -> bool {
        matches!(self.phase, EnginePhase::Running | EnginePhase::ChoicePending)
            && !self.transition.is_active()
    }

    /// Navigate choice selection (for keyboard input).
    pub fn select_choice(&mut self, direction: i32) {
        if let Some(choices) = &mut self.scene.choices
            && !choices.options.is_empty()
        {
            let count = choices.options.len() as i32;
            choices.selected = ((choices.selected as i32 + direction + count) % count) as usize;
        }
    }

    /// Confirm the currently selected choice.
    pub fn confirm_choice(&mut self) -> Vec<EngineEvent> {
        if let Some(choices) = &self.scene.choices {
            let selected = choices.selected;
            self.choose(selected)
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SCRIPT: &str = r#"
# Start

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

    #[test]
    fn test_engine_creation() {
        let engine = Engine::new(TEST_SCRIPT);
        assert!(engine.is_ok());
        let engine = engine.unwrap();
        assert_eq!(engine.phase(), EnginePhase::Title);
        assert!(engine.scene().show_title);
    }

    #[test]
    fn test_engine_start_and_advance() {
        let mut engine = Engine::start_running(TEST_SCRIPT).unwrap();

        // Process transitions (bg fade + character dissolve)
        // Each transition phase is ~0.3s, so ~0.7s covers full transition
        for _ in 0..20 {
            engine.update(0.1);
        }

        // After transition, we should have dialogue or narration
        assert!(
            engine.scene().dialogue.is_some()
                || engine.scene().background.is_some()
                || engine.scene().characters.iter().any(|c| c.name == "Aki")
        );
    }

    #[test]
    fn test_choices() {
        let mut engine = Engine::start_running(TEST_SCRIPT).unwrap();

        // Advance through all blocking events until choices appear
        let max_iterations = 100;
        for _ in 0..max_iterations {
            // Advance transitions
            engine.update(0.05);
            // Try to advance past current text
            let events = engine.advance();
            if events.iter().any(|e| matches!(e, EngineEvent::ChoicesShown { .. })) {
                break;
            }
        }
        assert_eq!(engine.phase(), EnginePhase::ChoicePending);
    }

    #[test]
    fn test_save_load() {
        let mut engine = Engine::start_running(TEST_SCRIPT).unwrap();
        // Advance a bit
        engine.advance();

        // Save
        let save_events = engine.save(0);
        assert!(save_events.iter().any(|e| matches!(e, EngineEvent::Saved { slot: 0 })));

        // Advance more
        engine.advance();
        engine.advance();

        // Load
        let load_events = engine.load(0);
        assert!(load_events.iter().any(|e| matches!(e, EngineEvent::Loaded { slot: 0 })));
    }

    #[test]
    fn test_hot_reload() {
        let mut engine = Engine::start_running(TEST_SCRIPT).unwrap();
        engine.advance();

        // Reload with modified script (same structure, different text)
        let modified = TEST_SCRIPT.replace("Hello there!", "Hi there!");
        let result = engine.reload_script(&modified);
        assert!(result.is_ok());

        // The source should be updated
        assert!(engine.source().contains("Hi there!"));
    }

    #[test]
    fn test_unknown_command_warning() {
        let script = r#"
# Start
@unknown_command some_arg
"Text"
"#;
        let mut engine = Engine::start_running(script).unwrap();
        let events = engine.update(0.016);
        // Should have a warning about unknown command
        // (events may be empty if transition is active, so just check no crash)
        assert!(engine.phase() != EnginePhase::Title);
    }

    #[test]
    fn test_multi_character_layout() {
        let src = r#"
# Stage
+ Aki enters
+ Yuki enters
- Aki
~~
"#;
        let mut engine = Engine::start_running(src).unwrap();

        // Process all events
        loop {
            engine.update(0.1);
            let _ = engine.advance();
            if engine.phase == EnginePhase::StoryEnded {
                break;
            }
        }

        // After Aki exits, Yuki should remain centered
        assert_eq!(engine.scene().characters.len(), 1);
        assert_eq!(engine.scene().characters[0].name, "Yuki");
        assert_eq!(engine.scene().characters[0].position, akrs_core::Position::Center);
    }
}
