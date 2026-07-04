//! akrs-cli: Command-line tool for the Akizuki*Rustgal engine.
//!
//! Usage:
//!   akrs check <file.akrs>       Validate a script file
//!   akrs run <file.akrs>         Run a script in text mode
//!   akrs pack <dir>              Pack a game directory (coming soon)
//!   akrs translate init <file.akrs> <lang>  Generate translation skeleton
//!   akrs help                    Show this help

mod migrate;

use akrs_core::{compile, compile_with_resources, format_location};
use akrs_runtime::{Engine, EngineEvent, EnginePhase};
use std::env;
use std::io::{self, Write};
use std::path::Path;

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        print_help();
        std::process::exit(1);
    }

    match args[1].as_str() {
        "check" => {
            if args.len() < 3 {
                eprintln!("Usage: akrs check <file.akrs> [assets_dir]");
                std::process::exit(1);
            }
            let file = &args[2];
            let assets = args.get(3).map(|s| s.as_str());
            cmd_check(file, assets);
        }
        "run" => {
            if args.len() < 3 {
                eprintln!("Usage: akrs run <file.akrs>");
                std::process::exit(1);
            }
            let file = &args[2];
            cmd_run(file);
        }
        "pack" => {
            let pack_args: Vec<String> = args[2..].to_vec();
            match akrs_pack::run_pack_cli(&pack_args) {
                Ok(()) => {}
                Err(e) => {
                    eprintln!("Pack error: {}", e);
                    std::process::exit(1);
                }
            }
        }
        "migrate" => {
            if args.len() < 3 {
                eprintln!("Usage: akrs migrate <input.rpy> [output.akrs]");
                std::process::exit(1);
            }
            let input = &args[2];
            let output = args.get(3).map(|s| s.as_str());
            migrate::cmd_migrate(input, output);
        }
        "translate" => {
            if args.len() < 3 {
                eprintln!("Usage: akrs translate <subcommand> [args]");
                eprintln!("Subcommands:");
                eprintln!("  init <file.akrs> <lang>  Generate translation skeleton JSON");
                std::process::exit(1);
            }
            match args[2].as_str() {
                "init" => {
                    if args.len() < 5 {
                        eprintln!("Usage: akrs translate init <file.akrs> <lang_code>");
                        eprintln!("  Generate a translation skeleton JSON file.");
                        eprintln!("  Example: akrs translate init demo.akrs en-US");
                        std::process::exit(1);
                    }
                    let file = &args[3];
                    let lang = &args[4];
                    cmd_translate_init(file, lang);
                }
                other => {
                    eprintln!("Unknown translate subcommand: {}", other);
                    std::process::exit(1);
                }
            }
        }
        "help" | "--help" | "-h" => {
            print_help();
        }
        _ => {
            eprintln!("Unknown command: {}", args[1]);
            print_help();
            std::process::exit(1);
        }
    }
}

fn print_help() {
    println!("Akizuki*Rustgal CLI v1.0");
    println!();
    println!("Usage: akrs <command> [args]");
    println!();
    println!("Commands:");
    println!("  check <file.akrs> [assets_dir]  Validate a script file");
    println!("  run <file.akrs>                 Run a script in text mode");
    println!("  pack [options]                  Pack a game for distribution");
    println!("                                  (use 'akrs pack --help' for details)");
    println!("  migrate <input.rpy> [output.akrs]");
    println!("                                   Convert Ren'Py script to .akrs");
    println!("  translate init <file.akrs> <lang>");
    println!("                                   Generate translation skeleton JSON");
    println!("  help                            Show this help");
    println!();
    println!("Examples:");
    println!("  akrs check scripts/demo.akrs");
    println!("  akrs check scripts/demo.akrs assets/");
    println!("  akrs run scripts/demo.akrs");
}

/// Check a .akrs script file for errors.
fn cmd_check(file: &str, assets_dir: Option<&str>) {
    let source = match std::fs::read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: failed to read '{}': {}", file, e);
            std::process::exit(1);
        }
    };

    let (program, errors) = if let Some(assets) = assets_dir {
        let resources = scan_resources(Path::new(assets));
        compile_with_resources(&source, resources)
    } else {
        compile(&source)
    };

    let has_errors = errors.iter().any(|e| e.severity == akrs_core::ErrSeverity::Error);

    for err in &errors {
        let loc = format_location(&err.span);
        let severity_str = match err.severity {
            akrs_core::ErrSeverity::Error => "error",
            akrs_core::ErrSeverity::Warning => "warning",
            akrs_core::ErrSeverity::Note => "note",
        };

        match &err.hint {
            Some(hint) => println!("{}: {}:{}: {} (hint: {})", severity_str, file, loc, err.message, hint),
            None => println!("{}: {}:{}: {}", severity_str, file, loc, err.message),
        }
    }

    if has_errors {
        eprintln!("\n{} error(s), {} warning(s)",
            errors.iter().filter(|e| e.severity == akrs_core::ErrSeverity::Error).count(),
            errors.iter().filter(|e| e.severity == akrs_core::ErrSeverity::Warning).count(),
        );
        std::process::exit(1);
    } else {
        let warning_count = errors.iter().filter(|e| e.severity == akrs_core::ErrSeverity::Warning).count();
        println!("\n✓ Script is valid ({} warning(s))", warning_count);

        if let Some(prog) = &program {
            println!("  Sections: {}", prog.sections.len());
            for sec in &prog.sections {
                println!("    - {} ({} nodes)", sec.name, sec.nodes.len());
            }
        }
    }
}

/// Run a .akrs script in text mode.
fn cmd_run(file: &str) {
    let source = match std::fs::read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: failed to read '{}': {}", file, e);
            std::process::exit(1);
        }
    };

    let mut engine = match Engine::start_running(&source) {
        Ok(e) => e,
        Err(errors) => {
            for err in &errors {
                let loc = format_location(&err.span);
                match &err.hint {
                    Some(hint) => eprintln!("error: {}:{}: {} (hint: {})", file, loc, err.message, hint),
                    None => eprintln!("error: {}:{}: {}", file, loc, err.message),
                }
            }
            std::process::exit(1);
        }
    };

    println!("=== Akizuki*Rustgal Text Mode ===");
    println!("=== Press Enter to advance ===");
    println!();

    // Process initial events
    let mut events = engine.update(1.0);
    print_events(&engine, &events);

    loop {
        // Check if story ended
        if engine.phase() == EnginePhase::StoryEnded {
            println!("\n=== Story End ===");
            break;
        }

        // Read input
        print!("> ");
        io::stdout().flush().unwrap();
        let mut input = String::new();
        io::stdin().read_line(&mut input).unwrap();

        let input = input.trim();

        // Handle choice input
        if engine.phase() == EnginePhase::ChoicePending {
            if let Ok(idx) = input.parse::<usize>() {
                events = engine.choose(idx);
                // Advance through transitions
                for _ in 0..30 {
                    events.extend(engine.update(0.1));
                }
                print_events(&engine, &events);
                continue;
            } else {
                println!("Please enter a number.");
                continue;
            }
        }

        // Advance
        events = engine.advance();
        // Advance through transitions
        for _ in 0..30 {
            let update_events = engine.update(0.1);
            events.extend(update_events);
            if !engine.scene().transition.is_some() {
                break;
            }
        }
        print_events(&engine, &events);
    }
}

/// Print engine events to the console.
fn print_events(_engine: &Engine, events: &[EngineEvent]) {
    for event in events {
        match event {
            EngineEvent::DialogueShown { speaker, text } => {
                if speaker.is_empty() {
                    println!("{}", text);
                } else {
                    println!("{}: {}", speaker, text);
                }
            }
            EngineEvent::NarrationShown { text } => {
                println!("{}", text);
            }
            EngineEvent::BackgroundChanged { name } => {
                println!("[Background: {}]", name);
            }
            EngineEvent::CharacterEntered { name } => {
                println!("[{} entered]", name);
            }
            EngineEvent::CharacterExited { name } => {
                println!("[{} exited]", name);
            }
            EngineEvent::MusicChanged { name } => {
                if name.is_empty() {
                    println!("[Music stopped]");
                } else {
                    println!("[Music: {}]", name);
                }
            }
            EngineEvent::SoundPlayed { name } => {
                println!("[Sound: {}]", name);
            }
            EngineEvent::VoicePlayed { name } => {
                if name.is_empty() {
                    println!("[Voice stopped]");
                } else {
                    println!("[Voice: {}]", name);
                }
            }
            EngineEvent::TransitionStarted { kind } => {
                println!("[Transition: {:?}]", kind);
            }
            EngineEvent::StoryEnded => {
                println!("\n=== Story End ===");
            }
            EngineEvent::Warning { message } => {
                eprintln!("[Warning: {}]", message);
            }
            EngineEvent::Error { message } => {
                eprintln!("[Error: {}]", message);
            }
            EngineEvent::ChoicesShown { prompt, options } => {
                if let Some(p) = prompt {
                    println!("{}", p);
                }
                for (i, opt) in options.iter().enumerate() {
                    if opt.available {
                        println!("  [{}] {}", i, opt.text);
                    } else {
                        println!("  [{}] {} (locked)", i, opt.text);
                    }
                }
            }
            _ => {}
        }
    }
}

/// 从剧本生成翻译骨架 JSON。
fn cmd_translate_init(file: &str, lang: &str) {
    let source = match std::fs::read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: failed to read '{}': {}", file, e);
            std::process::exit(1);
        }
    };

    use akrs_runtime::Translator;

    let mut translator = Translator::new();
    let display_name = match lang {
        "zh-CN" => "简体中文",
        "zh-TW" => "繁體中文",
        "en-US" => "English",
        "ja-JP" => "日本語",
        _ => lang,
    };
    translator.set_language(lang, display_name);

    let mut char_count = 0usize;
    let mut section_count = 0usize;
    let mut dialogue_count = 0usize;
    let mut narration_count = 0usize;
    let mut choice_count = 0usize;
    let mut prompt_count = 0usize;
    let mut chars_seen = std::collections::HashSet::new();

    for line in source.lines() {
        let trimmed = line.trim();
        // 章节标题
        if let Some(rest) = trimmed.strip_prefix('#') {
            let title = rest.trim().to_string();
            if !title.is_empty() {
                translator.set_section(&title, "");
                section_count += 1;
            }
        }
        // 角色对话
        else if let Some(colon_pos) = trimmed.find(':') {
            let (speaker_part, rest) = trimmed.split_at(colon_pos);
            let after_colon = rest[1..].trim();
            let speaker = if let Some(paren_pos) = speaker_part.find('(') {
                speaker_part[..paren_pos].trim().to_string()
            } else {
                speaker_part.trim().to_string()
            };
            if after_colon.starts_with('"') && after_colon.ends_with('"') && after_colon.len() >= 2 {
                let text = after_colon[1..after_colon.len() - 1].to_string();
                if !speaker.is_empty() && chars_seen.insert(speaker.clone()) {
                    translator.set_character(&speaker, "");
                    char_count += 1;
                }
                translator.set_dialogue(&text, "");
                dialogue_count += 1;
            }
        }
        // 旁白
        else if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2 {
            let text = trimmed[1..trimmed.len() - 1].to_string();
            translator.set_narration(&text, "");
            narration_count += 1;
        }
        // 选项提示
        else if trimmed.starts_with('?') {
            let rest = trimmed[1..].trim();
            if rest.starts_with('"') && rest.ends_with('"') && rest.len() >= 2 {
                let prompt = rest[1..rest.len() - 1].to_string();
                translator.set_choice_prompt(&prompt, "");
                prompt_count += 1;
            }
        }
        // 选项
        else if trimmed.starts_with('|') {
            let rest = trimmed[1..].trim();
            if rest.starts_with('"') {
                if let Some(end_quote) = rest[1..].find('"') {
                    let text = rest[1..end_quote + 1].to_string();
                    translator.set_choice(&text, "");
                    choice_count += 1;
                }
            }
        }
    }

    match translator.to_json() {
        Ok(json) => println!("{}", json),
        Err(e) => {
            eprintln!("error: {}", e);
            std::process::exit(1);
        }
    }

    eprintln!();
    eprintln!("✓ Generated translation skeleton for '{}'", lang);
    eprintln!("  Sections: {}", section_count);
    eprintln!("  Dialogue: {}", dialogue_count);
    eprintln!("  Narration: {}", narration_count);
    eprintln!("  Choices: {}", choice_count);
    eprintln!("  Choice prompts: {}", prompt_count);
    eprintln!("  Characters: {}", char_count);
    eprintln!();
    eprintln!("  Save to file: akrs translate init {} {} > {}.json", file, lang, lang);
}

/// Scan a directory for resource files.
fn scan_resources(dir: &Path) -> Vec<String> {
    let mut resources = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                resources.extend(scan_resources(&path));
            } else if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                resources.push(name.to_string());
                if let Ok(rel) = path.strip_prefix(dir)
                    && let Some(rel_str) = rel.to_str()
                {
                    resources.push(rel_str.to_string());
                }
            }
        }
    }
    resources
}
