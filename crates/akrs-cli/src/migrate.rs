//! Migration tool: Ren'Py (.rpy) → Akizuki*Rustgal (.akrs)
//!
//! Converts Ren'Py script syntax to .akrs syntax, collecting character
//! name mappings and image resource mappings along the way.

use std::collections::HashMap;

/// Result of a migration operation.
pub struct MigrationResult {
    /// The converted .akrs script text.
    pub akrs_script: String,
    /// Warnings for unconvertible parts.
    pub warnings: Vec<String>,
    /// Character short-name → full-name mappings (e.g. "e" → "Eileen").
    pub character_map: Vec<(String, String)>,
    /// Image logical-name → file-name mappings.
    pub image_map: Vec<(String, String)>,
}

/// A single line of Ren'Py source with its indentation level.
struct IndentedLine<'a> {
    indent: usize,
    content: &'a str,
    line_num: usize,
}

/// Parse Ren'Py source into indented lines, skipping blank lines.
fn parse_lines(source: &str) -> Vec<IndentedLine<'_>> {
    source
        .lines()
        .enumerate()
        .filter(|(_, line)| {
            let trimmed = line.trim();
            !trimmed.is_empty()
        })
        .map(|(i, line)| {
            let indent = line.len() - line.trim_start().len();
            IndentedLine {
                indent,
                content: line.trim(),
                line_num: i + 1,
            }
        })
        .collect()
}

/// Extract a quoted string from text, handling both " and ' quotes.
/// Returns (quoted_string, remainder).
fn extract_quoted_string(text: &str) -> Option<(String, &str)> {
    let text = text.trim_start();
    if text.is_empty() {
        return None;
    }
    let quote_char = text.chars().next()?;
    if quote_char != '"' && quote_char != '\'' {
        return None;
    }
    // Find matching closing quote (simple: no escape handling needed for typical rpy)
    let rest = &text[1..]; // skip opening quote
    let end = rest.find(quote_char)?;
    let string_content = &rest[..end];
    let remainder = &rest[end + 1..];
    Some((string_content.to_string(), remainder))
}

/// Check if a line is a comment (starts with #).
fn is_comment(line: &str) -> bool {
    line.trim_start().starts_with('#')
}

/// Check if a line starts with a keyword.
fn starts_with_keyword(line: &str, keyword: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed == keyword
        || trimmed.starts_with(&format!("{} ", keyword))
        || trimmed.starts_with(&format!("{}(", keyword))
        || trimmed.starts_with(&format!("{}:", keyword))
}

/// Convert a Ren'Py condition expression to .akrs syntax.
/// Ren'Py uses Python-like syntax: `x == 1`, `x > 0 and y < 10`
/// .akrs uses similar syntax, so mostly pass-through.
fn convert_condition(expr: &str) -> String {
    let mut result = expr.trim().to_string();
    // Ren'Py "not" → .akrs "!" (but .akrs also supports not? Let's keep as-is for simplicity)
    // Ren'Py "and" → .akrs "&&"? Actually .akrs uses "and"/"or" keywords too based on BinOp
    // Keep Python-style as-is since .akrs parser handles similar expressions
    // Remove trailing colon if present
    if result.ends_with(':') {
        result.pop();
    }
    result.trim().to_string()
}

/// Convert a Ren'Py expression to .akrs variable expression.
fn convert_expr(expr: &str) -> String {
    let mut result = expr.trim().to_string();
    if result.ends_with(':') {
        result.pop();
    }
    result.trim().to_string()
}

/// Main conversion function.
pub fn convert_rpy_to_akrs(rpy_source: &str) -> MigrationResult {
    let lines = parse_lines(rpy_source);
    let mut output = String::new();
    let mut warnings = Vec::new();
    let mut character_map: HashMap<String, String> = HashMap::new();
    let mut image_map: HashMap<String, String> = HashMap::new();
    let mut default_vars: Vec<String> = Vec::new();

    let mut i = 0;
    while i < lines.len() {
        let line = &lines[i];
        let content = line.content;

        // Skip block-ending markers
        if content == "pass" {
            i += 1;
            continue;
        }

        // Comments
        if is_comment(content) {
            // Check if it's a pure comment (not a label or directive)
            let comment_text = content.trim_start_matches('#').trim();
            output.push_str(&format!("-- {}\n", comment_text));
            i += 1;
            continue;
        }

        // define e = Character("Eileen")
        if starts_with_keyword(content, "define") {
            handle_define(content, &mut character_map, &mut image_map, &mut warnings, line.line_num);
            i += 1;
            continue;
        }

        // image bg_name = "file.png"
        if starts_with_keyword(content, "image") {
            handle_image(content, &mut image_map, &mut warnings, line.line_num);
            i += 1;
            continue;
        }

        // default var = value
        if starts_with_keyword(content, "default") {
            if let Some(var_line) = handle_default(content) {
                default_vars.push(var_line);
            }
            i += 1;
            continue;
        }

        // init python: block — skip entirely
        if starts_with_keyword(content, "init") || (content.starts_with("init") && content.contains("python")) {
            warnings.push(format!("line {}: init python block skipped", line.line_num));
            i = skip_block(&lines, i);
            continue;
        }

        // screen name: block — skip
        if starts_with_keyword(content, "screen") {
            warnings.push(format!("line {}: screen definition skipped: {}", line.line_num, content));
            i = skip_block(&lines, i);
            continue;
        }

        // transform name: block — skip
        if starts_with_keyword(content, "transform") {
            warnings.push(format!("line {}: transform definition skipped: {}", line.line_num, content));
            i = skip_block(&lines, i);
            continue;
        }

        // renpy.* calls
        if content.contains("renpy.") {
            warnings.push(format!("line {}: renpy API call skipped: {}", line.line_num, content.trim()));
            i += 1;
            continue;
        }

        // label Name:
        if starts_with_keyword(content, "label") {
            let name = content
                .trim_start_matches("label")
                .trim()
                .trim_end_matches(':')
                .trim();
            // Remove any parentheses or arguments (e.g. "label start(quiet=True):")
            let name = name.split('(').next().unwrap_or(name).trim();
            output.push_str(&format!("# {}\n", name));
            i += 1;
            continue;
        }

        // jump Name
        if starts_with_keyword(content, "jump") {
            let target = content
                .trim_start_matches("jump")
                .trim()
                .trim_end_matches(':');
            output.push_str(&format!("-> {}\n", target));
            i += 1;
            continue;
        }

        // call Name
        if starts_with_keyword(content, "call") {
            let target = content
                .trim_start_matches("call")
                .trim()
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_end_matches(':');
            output.push_str(&format!("=> {}\n", target));
            i += 1;
            continue;
        }

        // return
        if starts_with_keyword(content, "return") {
            output.push_str("<=\n");
            i += 1;
            continue;
        }

        // scene bg_name [with transition]
        if starts_with_keyword(content, "scene") {
            let rest = content.trim_start_matches("scene").trim();
            let parts: Vec<&str> = rest.splitn(2, " with ").collect();
            let bg_name = parts[0].trim();
            // Remove common prefixes like "bg " or "image "
            let bg_name = bg_name.trim_start_matches("bg ").trim();
            let transition = parts.get(1).map(|s| *s).unwrap_or("");
            if transition.is_empty() {
                output.push_str(&format!("@bg {}\n", bg_name));
            } else {
                output.push_str(&format!("@bg {} with {}\n", bg_name, convert_transition(transition)));
            }
            i += 1;
            continue;
        }

        // show character_name [expression] [at position] [with transition]
        if starts_with_keyword(content, "show") {
            let rest = content.trim_start_matches("show").trim();
            // Split off "with transition" part
            let parts: Vec<&str> = rest.splitn(2, " with ").collect();
            let main_part = parts[0].trim();
            let transition = parts.get(1).map(|s| *s).unwrap_or("");

            let tokens: Vec<&str> = main_part.split_whitespace().collect();
            if tokens.is_empty() {
                i += 1;
                continue;
            }
            let char_name = tokens[0];
            // Expression is the second token if present
            let expression = tokens.get(1).filter(|t| !["at", "with", "as"].contains(t));
            // Position
            let position = tokens.iter().position(|&t| t == "at").and_then(|pos| tokens.get(pos + 1));

            // Resolve character name from map
            let resolved_name = character_map.get(char_name).cloned().unwrap_or_else(|| char_name.to_string());

            let mut dir = format!("+ {}", resolved_name);
            if let Some(expr) = expression {
                dir.push_str(&format!(" {}", expr));
            }
            dir.push_str(" enters");
            if let Some(pos) = position {
                dir.push_str(&format!(" from {}", pos));
            }
            if !transition.is_empty() {
                dir.push_str(&format!(" with {}", convert_transition(transition)));
            } else {
                dir.push_str(" with dissolve");
            }
            output.push_str(&format!("{}\n", dir));
            i += 1;
            continue;
        }

        // hide character_name [with transition]
        if starts_with_keyword(content, "hide") {
            let rest = content.trim_start_matches("hide").trim();
            let parts: Vec<&str> = rest.splitn(2, " with ").collect();
            let char_name = parts[0].trim().split_whitespace().next().unwrap_or("");
            let resolved_name = character_map.get(char_name).cloned().unwrap_or_else(|| char_name.to_string());
            let transition = parts.get(1).map(|s| *s).unwrap_or("");
            if transition.is_empty() {
                output.push_str(&format!("- {}\n", resolved_name));
            } else {
                output.push_str(&format!("- {} with {}\n", resolved_name, convert_transition(transition)));
            }
            i += 1;
            continue;
        }

        // menu [prompt]:
        if starts_with_keyword(content, "menu") {
            let rest = content.trim_start_matches("menu").trim().trim_end_matches(':').trim();
            let mut prompt: Option<String> = if rest.starts_with('"') || rest.starts_with('\'') {
                extract_quoted_string(rest).map(|(s, _)| s)
            } else if !rest.is_empty() {
                Some(rest.to_string())
            } else {
                None
            };

            i += 1;
            let menu_indent = line.indent;

            // If no prompt on the menu line, check if the first indented line
            // is a narration string (quoted string without ":") — treat as prompt
            if prompt.is_none() && i < lines.len() && lines[i].indent > menu_indent {
                let first_content = lines[i].content;
                if let Some((text, remainder)) = extract_quoted_string(first_content) {
                    let remainder = remainder.trim();
                    // If remainder doesn't start with ":" or "if", it's narration, not an option
                    if !remainder.starts_with(':') && !remainder.starts_with("if") {
                        prompt = Some(text);
                        i += 1; // consume the narration line as prompt
                    }
                }
            }

            if let Some(p) = &prompt {
                output.push_str(&format!("? \"{}\"\n", p));
            } else {
                output.push_str("? \"\"\n");
            }

            // Process menu options (indented under menu)
            while i < lines.len() && lines[i].indent > menu_indent {
                let opt_line = &lines[i];
                let opt_content = opt_line.content;

                // Option line: "text": or "text" if condition:
                if let Some((text, remainder)) = extract_quoted_string(opt_content) {
                    let remainder = remainder.trim();
                    // Check for condition: "text" if condition:
                    let condition = if remainder.starts_with("if") {
                        let cond_part = remainder.trim_start_matches("if").trim();
                        Some(convert_condition(cond_part))
                    } else {
                        None
                    };

                    if let Some(cond) = condition {
                        output.push_str(&format!("| \"{}\" if {}\n", text, cond));
                    } else {
                        output.push_str(&format!("| \"{}\"\n", text));
                    }

                    // Process option body (further indented)
                    i += 1;
                    let opt_indent = opt_line.indent;
                    while i < lines.len() && lines[i].indent > opt_indent {
                        let body_line = &lines[i];
                        let body_content = body_line.content;
                        // Convert body lines (they might be jumps, sets, etc.)
                        convert_simple_line(body_content, &mut output, &character_map, body_line.line_num);
                        i += 1;
                    }
                } else {
                    // Non-option line inside menu, just convert
                    convert_simple_line(opt_content, &mut output, &character_map, opt_line.line_num);
                    i += 1;
                }
            }
            output.push_str("?\n");
            continue;
        }

        // if / elif / else
        if starts_with_keyword(content, "if") || starts_with_keyword(content, "elif") || starts_with_keyword(content, "else") {
            if starts_with_keyword(content, "if") {
                let cond = content.trim_start_matches("if").trim();
                output.push_str(&format!("if {}\n", convert_condition(cond)));
            } else if starts_with_keyword(content, "elif") {
                let cond = content.trim_start_matches("elif").trim();
                output.push_str(&format!("else if {}\n", convert_condition(cond)));
            } else {
                // else
                output.push_str("else\n");
            }
            i += 1;
            // Process body
            let block_indent = line.indent;
            while i < lines.len() && lines[i].indent > block_indent {
                let body_line = &lines[i];
                // Handle nested if/elif/else
                if starts_with_keyword(body_line.content, "elif") || starts_with_keyword(body_line.content, "else") {
                    break;
                }
                convert_simple_line(body_line.content, &mut output, &character_map, body_line.line_num);
                i += 1;
            }
            // Check for elif/else at same indent
            while i < lines.len() && lines[i].indent == block_indent {
                let next_line = &lines[i];
                if starts_with_keyword(next_line.content, "elif") {
                    let cond = next_line.content.trim_start_matches("elif").trim();
                    output.push_str(&format!("else if {}\n", convert_condition(cond)));
                } else if starts_with_keyword(next_line.content, "else") {
                    output.push_str("else\n");
                } else {
                    break;
                }
                i += 1;
                // Process body
                while i < lines.len() && lines[i].indent > block_indent {
                    let body_line = &lines[i];
                    convert_simple_line(body_line.content, &mut output, &character_map, body_line.line_num);
                    i += 1;
                }
            }
            output.push_str("end\n");
            continue;
        }

        // while condition:
        if starts_with_keyword(content, "while") {
            let cond = content.trim_start_matches("while").trim();
            // .akrs doesn't have native while; use a label + jump back pattern
            // We'll generate a unique loop label
            let loop_label = format!("__loop_{}", line.line_num);
            output.push_str(&format!("# {}\n", loop_label));
            output.push_str(&format!("if {}\n", convert_condition(cond)));
            i += 1;
            let block_indent = line.indent;
            while i < lines.len() && lines[i].indent > block_indent {
                let body_line = &lines[i];
                convert_simple_line(body_line.content, &mut output, &character_map, body_line.line_num);
                i += 1;
            }
            output.push_str(&format!("-> {}\n", loop_label));
            output.push_str("end\n");
            continue;
        }

        // Dialogue: character "text" or just "text"
        // Check if it starts with a quoted string (narration)
        if content.starts_with('"') || content.starts_with('\'') {
            if let Some((text, _)) = extract_quoted_string(content) {
                output.push_str(&format!("\"{}\"\n", text));
            }
            i += 1;
            continue;
        }

        // Check for character dialogue: `e "text"` or `character "text"`
        // The pattern is: identifier followed by space and quoted string
        if let Some(space_pos) = content.find(' ') {
            let first_word = &content[..space_pos];
            let rest = content[space_pos..].trim();
            if (rest.starts_with('"') || rest.starts_with('\'')) && !first_word.contains('(') {
                if let Some((text, _)) = extract_quoted_string(rest) {
                    // Resolve character name
                    let speaker = character_map.get(first_word).cloned().unwrap_or_else(|| first_word.to_string());
                    output.push_str(&format!("{}: \"{}\"\n", speaker, text));
                    i += 1;
                    continue;
                }
            }
        }

        // $ variable = value (Ren'Py inline Python)
        if content.starts_with('$') {
            let expr = content.trim_start_matches('$').trim();
            output.push_str(&format!("${}\n", convert_expr(expr)));
            i += 1;
            continue;
        }

        // play music / stop music
        if starts_with_keyword(content, "play") {
            let rest = content.trim_start_matches("play").trim();
            if rest.starts_with("music") {
                let music_name = rest.trim_start_matches("music").trim();
                if let Some((name, _)) = extract_quoted_string(music_name) {
                    output.push_str(&format!("@music {}\n", name));
                }
            } else if rest.starts_with("sound") {
                let sound_name = rest.trim_start_matches("sound").trim();
                if let Some((name, _)) = extract_quoted_string(sound_name) {
                    output.push_str(&format!("@sound {}\n", name));
                }
            }
            i += 1;
            continue;
        }

        if starts_with_keyword(content, "stop") {
            let rest = content.trim_start_matches("stop").trim();
            if rest.starts_with("music") {
                output.push_str("@stop_music\n");
            }
            i += 1;
            continue;
        }

        // with transition (standalone)
        if starts_with_keyword(content, "with") {
            // Skip standalone with statements (transitions are handled inline)
            i += 1;
            continue;
        }

        // $end or end — skip
        if content == "end" || content == "$end" {
            i += 1;
            continue;
        }

        // Unknown line — emit as comment with warning
        warnings.push(format!("line {}: unrecognized syntax, emitted as comment: {}", line.line_num, content));
        output.push_str(&format!("-- TODO: {}\n", content));
        i += 1;
    }

    // Build final script: default vars first, then body
    let mut final_script = String::new();
    if !default_vars.is_empty() {
        final_script.push_str("-- Default variables (from Ren'Py 'default' declarations)\n");
        for var in &default_vars {
            final_script.push_str(var);
            final_script.push('\n');
        }
        final_script.push('\n');
    }
    final_script.push_str(&output);

    MigrationResult {
        akrs_script: final_script,
        warnings,
        character_map: character_map.into_iter().collect(),
        image_map: image_map.into_iter().collect(),
    }
}

/// Handle `define` statements.
fn handle_define(
    content: &str,
    character_map: &mut HashMap<String, String>,
    _image_map: &mut HashMap<String, String>,
    _warnings: &mut Vec<String>,
    _line_num: usize,
) {
    let rest = content.trim_start_matches("define").trim();
    // define e = Character("Eileen")
    if let Some(eq_pos) = rest.find('=') {
        let name = rest[..eq_pos].trim();
        let value = rest[eq_pos + 1..].trim();
        if value.starts_with("Character") {
            // After "Character", skip whitespace and opening parenthesis to find the quote
            let after_char = value.trim_start_matches("Character").trim();
            // Remove leading '('
            let after_paren = after_char.trim_start_matches('(').trim();
            if let Some((char_name, _)) = extract_quoted_string(after_paren) {
                character_map.insert(name.to_string(), char_name);
            }
        }
    }
}

/// Handle `image` statements.
fn handle_image(
    content: &str,
    image_map: &mut HashMap<String, String>,
    _warnings: &mut Vec<String>,
    _line_num: usize,
) {
    let rest = content.trim_start_matches("image").trim();
    // image bg_name = "file.png"
    if let Some(eq_pos) = rest.find('=') {
        let name = rest[..eq_pos].trim();
        let value = rest[eq_pos + 1..].trim();
        if let Some((file, _)) = extract_quoted_string(value) {
            image_map.insert(name.to_string(), file);
        }
    }
}

/// Handle `default` statements. Returns a `$var = value` line if applicable.
fn handle_default(content: &str) -> Option<String> {
    let rest = content.trim_start_matches("default").trim();
    if let Some(eq_pos) = rest.find('=') {
        let name = rest[..eq_pos].trim();
        let value = rest[eq_pos + 1..].trim();
        // Convert common Python values
        let value = value.trim_end_matches(':');
        Some(format!("${} = {}", name, value))
    } else {
        None
    }
}

/// Convert a simple line (non-block) to .akrs syntax.
fn convert_simple_line(
    content: &str,
    output: &mut String,
    character_map: &HashMap<String, String>,
    _line_num: usize,
) {
    let content = content.trim();
    if content.is_empty() || content == "pass" {
        return;
    }

    if is_comment(content) {
        let comment_text = content.trim_start_matches('#').trim();
        output.push_str(&format!("-- {}\n", comment_text));
        return;
    }

    if starts_with_keyword(content, "jump") {
        let target = content.trim_start_matches("jump").trim().trim_end_matches(':');
        output.push_str(&format!("-> {}\n", target));
        return;
    }

    if starts_with_keyword(content, "call") {
        let target = content
            .trim_start_matches("call")
            .trim()
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_end_matches(':');
        output.push_str(&format!("=> {}\n", target));
        return;
    }

    if starts_with_keyword(content, "return") {
        output.push_str("<=\n");
        return;
    }

    if starts_with_keyword(content, "scene") {
        let rest = content.trim_start_matches("scene").trim();
        let parts: Vec<&str> = rest.splitn(2, " with ").collect();
        let bg_name = parts[0].trim().trim_start_matches("bg ").trim();
        let transition = parts.get(1).map(|s| *s).unwrap_or("");
        if transition.is_empty() {
            output.push_str(&format!("@bg {}\n", bg_name));
        } else {
            output.push_str(&format!("@bg {} with {}\n", bg_name, convert_transition(transition)));
        }
        return;
    }

    if starts_with_keyword(content, "show") {
        let rest = content.trim_start_matches("show").trim();
        let parts: Vec<&str> = rest.splitn(2, " with ").collect();
        let main_part = parts[0].trim();
        let transition = parts.get(1).map(|s| *s).unwrap_or("");
        let tokens: Vec<&str> = main_part.split_whitespace().collect();
        if tokens.is_empty() {
            return;
        }
        let char_name = tokens[0];
        let resolved_name = character_map.get(char_name).cloned().unwrap_or_else(|| char_name.to_string());
        let mut dir = format!("+ {} enters", resolved_name);
        if !transition.is_empty() {
            dir.push_str(&format!(" with {}", convert_transition(transition)));
        } else {
            dir.push_str(" with dissolve");
        }
        output.push_str(&format!("{}\n", dir));
        return;
    }

    if starts_with_keyword(content, "hide") {
        let rest = content.trim_start_matches("hide").trim();
        let char_name = rest.split_whitespace().next().unwrap_or("");
        let resolved_name = character_map.get(char_name).cloned().unwrap_or_else(|| char_name.to_string());
        output.push_str(&format!("- {}\n", resolved_name));
        return;
    }

    // Dialogue
    if content.starts_with('"') || content.starts_with('\'') {
        if let Some((text, _)) = extract_quoted_string(content) {
            output.push_str(&format!("\"{}\"\n", text));
        }
        return;
    }

    // Character dialogue
    if let Some(space_pos) = content.find(' ') {
        let first_word = &content[..space_pos];
        let rest = content[space_pos..].trim();
        if (rest.starts_with('"') || rest.starts_with('\'')) && !first_word.contains('(') {
            if let Some((text, _)) = extract_quoted_string(rest) {
                let speaker = character_map.get(first_word).cloned().unwrap_or_else(|| first_word.to_string());
                output.push_str(&format!("{}: \"{}\"\n", speaker, text));
                return;
            }
        }
    }

    // $ variable
    if content.starts_with('$') {
        let expr = content.trim_start_matches('$').trim();
        output.push_str(&format!("${}\n", convert_expr(expr)));
        return;
    }

    // play/stop music
    if starts_with_keyword(content, "play") {
        let rest = content.trim_start_matches("play").trim();
        if rest.starts_with("music") {
            let music_name = rest.trim_start_matches("music").trim();
            if let Some((name, _)) = extract_quoted_string(music_name) {
                output.push_str(&format!("@music {}\n", name));
            }
        } else if rest.starts_with("sound") {
            let sound_name = rest.trim_start_matches("sound").trim();
            if let Some((name, _)) = extract_quoted_string(sound_name) {
                output.push_str(&format!("@sound {}\n", name));
            }
        }
        return;
    }

    if starts_with_keyword(content, "stop") {
        let rest = content.trim_start_matches("stop").trim();
        if rest.starts_with("music") {
            output.push_str("@stop_music\n");
        }
        return;
    }

    if starts_with_keyword(content, "with") {
        return; // skip
    }

    // Unknown — emit as comment
    output.push_str(&format!("-- TODO: {}\n", content));
}

/// Convert Ren'Py transition names to .akrs transition names.
fn convert_transition(name: &str) -> &str {
    let name = name.trim();
    match name {
        "fade" => "fade",
        "dissolve" => "dissolve",
        "fadeblack" | "fade_black" => "fade_black",
        "fadewhite" | "fade_white" => "fade_white",
        "slideleft" | "slide_left" => "slide_left",
        "slideright" | "slide_right" => "slide_right",
        "slideup" | "slide_up" => "slide_up",
        "slidedown" | "slide_down" => "slide_down",
        "wipeleft" | "wipe_left" => "wipe_left",
        "wiperight" | "wipe_right" => "wipe_right",
        "blur" => "blur",
        "none" | "instant" | "cut" => "instant",
        _ => "fade", // default to fade for unknown transitions
    }
}

/// Skip a block of indented lines (used for init python, screen, transform).
fn skip_block(lines: &[IndentedLine], start: usize) -> usize {
    let block_indent = lines[start].indent;
    let mut i = start + 1;
    while i < lines.len() && lines[i].indent > block_indent {
        i += 1;
    }
    i
}

/// CLI entry point for the migrate command.
pub fn cmd_migrate(input_path: &str, output_path: Option<&str>) {
    let source = match std::fs::read_to_string(input_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: failed to read '{}': {}", input_path, e);
            std::process::exit(1);
        }
    };

    let result = convert_rpy_to_akrs(&source);

    // Determine output path
    let out_path = output_path
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            // Replace .rpy extension with .akrs
            let p = std::path::Path::new(input_path);
            let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
            let parent = p.parent().unwrap_or(std::path::Path::new("."));
            parent.join(format!("{}.akrs", stem)).to_string_lossy().to_string()
        });

    // Write output
    if let Err(e) = std::fs::write(&out_path, &result.akrs_script) {
        eprintln!("error: failed to write '{}': {}", out_path, e);
        std::process::exit(1);
    }

    println!("Migration complete: {} → {}", input_path, out_path);
    println!("  Output: {} lines", result.akrs_script.lines().count());

    // Print character map
    if !result.character_map.is_empty() {
        eprintln!("\nCharacter name mappings:");
        for (short, full) in &result.character_map {
            eprintln!("  {} → {}", short, full);
        }
    }

    // Print image map
    if !result.image_map.is_empty() {
        eprintln!("\nImage resource mappings:");
        for (name, file) in &result.image_map {
            eprintln!("  {} → {}", name, file);
        }
    }

    // Print warnings
    if !result.warnings.is_empty() {
        eprintln!("\nWarnings ({}):", result.warnings.len());
        for w in &result.warnings {
            eprintln!("  ⚠ {}", w);
        }
    } else {
        eprintln!("\nNo warnings.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_label_jump_call_return() {
        let rpy = r#"
label start:
    "Hello world."
    e "Hi there!"
    jump next_scene
    call subroutine
    return

label next_scene:
    "Next scene."
    return

label subroutine:
    "In subroutine."
    return
"#;
        let result = convert_rpy_to_akrs(rpy);
        assert!(result.akrs_script.contains("# start"));
        assert!(result.akrs_script.contains("# next_scene"));
        assert!(result.akrs_script.contains("# subroutine"));
        assert!(result.akrs_script.contains("-> next_scene"));
        assert!(result.akrs_script.contains("=> subroutine"));
        assert!(result.akrs_script.contains("<="));
        assert!(result.akrs_script.contains("\"Hello world.\""));
    }

    #[test]
    fn test_scene_show_hide_menu() {
        let rpy = r#"
label scene1:
    scene bg school with fade
    show eileen happy at left with dissolve
    eileen "Welcome!"
    hide eileen with dissolve
    menu:
        "What do you want?"
        "Option A":
            jump ending_a
        "Option B":
            jump ending_b
"#;
        let result = convert_rpy_to_akrs(rpy);
        assert!(result.akrs_script.contains("@bg school with fade"));
        assert!(result.akrs_script.contains("+ eileen"));
        assert!(result.akrs_script.contains("enters"));
        assert!(result.akrs_script.contains("- eileen"));
        assert!(result.akrs_script.contains("? \"What do you want?\""));
        assert!(result.akrs_script.contains("| \"Option A\""));
        assert!(result.akrs_script.contains("| \"Option B\""));
        assert!(result.akrs_script.contains("-> ending_a"));
        assert!(result.akrs_script.contains("-> ending_b"));
        // Menu block should end with ?
        assert!(result.akrs_script.contains("?\n"));
    }

    #[test]
    fn test_define_image_default_if() {
        let rpy = r#"
define e = Character("Eileen")
image bg_school = "images/school.png"
default affection = 0

label start:
    scene bg_school
    show eileen happy
    e "Hello!"
    $affection += 1
    if affection > 5:
        e "You like me!"
    else:
        e "Do you like me?"
    end
"#;
        let result = convert_rpy_to_akrs(rpy);

        // Character map
        assert!(result.character_map.iter().any(|(s, f)| s == "e" && f == "Eileen"));

        // Image map
        assert!(result.image_map.iter().any(|(n, f)| n == "bg_school" && f == "images/school.png"));

        // Default vars collected at top
        assert!(result.akrs_script.starts_with("-- Default variables"));
        assert!(result.akrs_script.contains("$affection = 0"));

        // Character name resolved
        assert!(result.akrs_script.contains("Eileen: \"Hello!\""));

        // Variable operation
        assert!(result.akrs_script.contains("$affection += 1"));

        // Conditional
        assert!(result.akrs_script.contains("if affection > 5"));
        assert!(result.akrs_script.contains("Eileen: \"You like me!\""));
        assert!(result.akrs_script.contains("else"));
        assert!(result.akrs_script.contains("Eileen: \"Do you like me?\""));
        assert!(result.akrs_script.contains("end"));
    }

    #[test]
    fn test_comments_and_narration() {
        let rpy = r#"
# This is a comment
label start:
    "This is narration."
    e "This is dialogue."
    # Another comment
    return
"#;
        let result = convert_rpy_to_akrs(rpy);
        assert!(result.akrs_script.contains("-- This is a comment"));
        assert!(result.akrs_script.contains("\"This is narration.\""));
    }

    #[test]
    fn test_skip_blocks() {
        let rpy = r#"
init python:
    config.developer = True

screen preferences():
    text "Settings"

label start:
    "Game starts."
    return
"#;
        let result = convert_rpy_to_akrs(rpy);
        // Should skip init python and screen blocks
        assert!(result.warnings.iter().any(|w| w.contains("init python")));
        assert!(result.warnings.iter().any(|w| w.contains("screen")));
        // But should still convert the label
        assert!(result.akrs_script.contains("# start"));
        assert!(result.akrs_script.contains("\"Game starts.\""));
    }

    #[test]
    fn test_while_loop() {
        let rpy = r#"
label loop_test:
    $count = 0
    while count < 3:
        e "Count is increasing."
        $count += 1
    return
"#;
        let result = convert_rpy_to_akrs(rpy);
        // While is converted to label + if + jump pattern
        assert!(result.akrs_script.contains("if count < 3"));
        assert!(result.akrs_script.contains("$count += 1"));
        // Should have a jump back to the loop label
        assert!(result.akrs_script.contains("-> __loop_"));
    }
}
