> **语言 / Language:** [简体中文](翻译功能使用说明.md) | [English](translation-guide.en.md) | [日本語](translation-guide.ja.md) | [繁體中文](translation-guide.zh-TW.md)

# Translation Feature Usage Guide

The Akizuki\*Rustgal engine has a built-in complete multilingual translation system that adopts the "original text as key" strategy. **The original script does not need to be modified at all** — just add translation files to support any language.

---

## Design Principles

- **Original text as key** — Translation files use the original text as the key and the translation as the value; the original script is not modified at all.
- **Injection point at the Engine layer** — The VM and compiler require zero changes; switching languages does not require recompilation and does not affect save files.
- **Display-layer translation, logic-layer unchanged** — Character names, section titles, etc. always use the original text in the logic layer (jumps, stage management, save metadata), and are only looked up and replaced before being displayed to the player.
- **Fallback to original text** — When a translation is not found or is empty, the original text is returned directly; the game will not error out due to missing translations.
- **Zero hot-path allocation** — Lookup is `HashMap::get` with amortized O(1), with no memory allocation.

---

## Translation File Format

Translation files are in JSON format, stored in the `assets/scripts/languages/` directory, with the filename being the language code (e.g. `ja-JP.json`).

```json
{
  "language": "ja-JP",
  "display_name": "日本語",
  "sections": {
    "夏日祭的尾声": "夏祭りの終わりに"
  },
  "dialogue": {
    "引航者，今天晚上玩得开心吗？": "ナビゲーター、今夜は楽しかった？"
  },
  "narration": {
    "心夏低下了头。": "心夏は頭を下げた。"
  },
  "choices": {
    "我有个小礼物想送给心夏": "心夏にちょっとしたプレゼントがある"
  },
  "choice_prompts": {
    "引航者要对心夏说什么？": "ナビゲーターは心夏に何を言う？"
  },
  "characters": {
    "心夏": "心夏",
    "引航者": "ナビゲーター"
  }
}
```

### Field Descriptions

| Field | Type | Description |
|------|------|------|
| `language` | string | Language code, e.g. `zh-CN`, `en-US`, `ja-JP` |
| `display_name` | string | Language display name, e.g. `简体中文`, `English`, `日本語` |
| `sections` | object | Section title translations (lines starting with `#`) |
| `dialogue` | object | Dialogue text translations (lines in `Character name: "text"` format) |
| `narration` | object | Narration text translations (standalone quoted lines) |
| `choices` | object | Branch choice text translations (lines starting with `\|`) |
| `choice_prompts` | object | Choice prompt translations (lines starting with `?`) |
| `characters` | object | Character name translations (display-layer replacement only; the logic layer uses the original text) |

> **Empty translation**: An empty string value `""` means "untranslated"; the runtime will fall back to displaying the original text.

---

## Translatable Content

The translation system covers the following six categories of script elements:

| Category | Script Syntax | Example |
|------|----------|------|
| Section title | `# Title` | `# 夏日祭的尾声` |
| Dialogue | `Character name: "text"` | `心夏: "今天开心吗？"` |
| Narration | `"text"` | `"心夏低下了头。"` |
| Choice prompt | `? "prompt"` | `? "你要说什么？"` |
| Choice | `\| "text" -> jump` | `\| "送礼物" -> 分支A` |
| Character name | The part before the colon in a dialogue line | `心夏` |

---

## Generating a Translation Skeleton (CLI)

Use the command-line tool to automatically scan the script and generate a JSON skeleton file containing all translatable text:

```bash
# Basic usage
akrs translate init <script_file> <language_code>

# Example: generate a Japanese translation skeleton for demo.akrs
akrs translate init scripts/demo.akrs ja-JP > assets/scripts/languages/ja-JP.json

# Generate an English translation skeleton
akrs translate init scripts/demo.akrs en-US > assets/scripts/languages/en-US.json
```

### Supported Language Codes

| Code | Display Name |
|------|--------|
| `zh-CN` | 简体中文 |
| `zh-TW` | 繁體中文 |
| `en-US` | English |
| `ja-JP` | 日本語 |

Other language codes can also be used; `display_name` will default to the language code itself, and can be manually edited in the generated JSON.

### Output Example

```
✓ Generated translation skeleton for 'ja-JP'
  Sections: 4
  Dialogue: 40
  Narration: 5
  Choices: 2
  Choice prompts: 1
  Characters: 2
```

All translations in the generated JSON are empty strings `""`; translators only need to fill in the corresponding translations.

---

## Editor Side-by-Side Translation Mode

The editor has a built-in side-by-side translation feature, providing an original-text/translation parallel view to make it easier for translators to translate line by line.

### How to Enable

Click the **"Side-by-Side Translation"** button on the editor toolbar to toggle translation mode.

### Interface Description

```
┌─────────────────────────────────────────────┐
│ 目标语言：[ja-JP]  [加载] [重新提取] [保存]  │
│ 共 52 行可翻译                                │
├─────────────────────────────────────────────┤
│ [对话]  行 92   #1                           │
│ ┌─ 原文 ──────────────────────────────────┐ │
│ │ 引航者，今天晚上玩得开心吗？               │ │
│ └────────────────────────────────────────┘ │
│ ┌─ 译文 ──────────────────────────────────┐ │
│ │ ナビゲーター、今夜は楽しかった？           │ │
│ └────────────────────────────────────────┘ │
│                                             │
│ [旁白]  行 97   #2                          │
│ ┌─ 原文 ──────────────────────────────────┐ │
│ │ 心夏低下了头。                           │ │
│ └────────────────────────────────────────┘ │
│ ┌─ 译文 ──────────────────────────────────┐ │
│ │ 心夏は頭を下げた。                       │ │
│ └────────────────────────────────────────┘ │
└─────────────────────────────────────────────┘
```

### Operation Steps

1. **Enter the target language code** (e.g. `ja-JP`)
2. **Click "Load"** — Load existing translations from `assets/scripts/languages/<language>.json` (if the file does not exist, a new empty translation table is created)
3. **Translate line by line** — Enter translations in the "Translation" input box; modifications are written to the translation table in real time
4. **Click "Save Translation"** — Serialize the translation table as JSON and save it to `assets/scripts/languages/<language>.json`

### Auxiliary Features

- **Type tags** — Each line is tagged with its type (`Dialogue`/`Narration`/`Section`/`Choice`/`Prompt`/`Character`); different types are distinguished by different colors
- **Line number display** — Shows the line number of the original text in the script for easy locating
- **Re-extract** — After the script is modified, click "Re-extract" to refresh the list of translatable text
- **Original text read-only** — The original text area is not editable, ensuring that translations are based on the correct original text

---

## Runtime Language Switching

### Switching via Settings Page

You can switch languages in real time via the **Language drop-down box** on the in-game settings page:

1. Open the settings page
2. Find the "Language" option
3. Select the target language from the drop-down box
4. The game applies the translation immediately, with no restart required

### Translation File Loading

At game startup, translations are loaded in the following order:

1. Read the `language` field in `ProjectConfig` (the project's default language)
2. If empty, detect the system language:
   - **Windows**: calls `GetUserDefaultLocaleName` to read the user locale (BCP 47 format)
   - **Linux/macOS**: reads environment variables in `LANGUAGE`→`LC_ALL`→`LC_MESSAGES`→`LANG` order
   - The detected locale is normalized to a supported language code (`zh-CN`/`zh-TW`/`en-US`/`ja-JP`)
3. Load the translation file from `assets/scripts/languages/<language_code>.json`
4. If the file does not exist, use original-text mode (all text is displayed in the original text)

### Project Configuration

In the "Run Settings" area of the editor's "Project Settings" dialog, you can configure:

| Field | Description |
|------|------|
| Default language | The language code used at project startup (e.g. `ja-JP`); if left blank, the system language is detected |
| Window title | The game window title; if left blank, the main title is used |
| Version number | The project version number (e.g. `v1.0.0`) |
| Default resolution | Startup resolution (default 1920×1080) |
| Fullscreen on startup | Whether to launch in fullscreen mode |

---

## Translation File Directory Structure

```
assets/
└── scripts/
    ├── demo.akrs                  Original script (not modified)
    └── languages/
        ├── zh-CN.json             Simplified Chinese translation
        ├── zh-TW.json             Traditional Chinese translation
        ├── en-US.json             English translation
        └── ja-JP.json             Japanese translation
```

> Each translation file corresponds one-to-one with the original script; to add a new language, simply add the corresponding JSON file in the `languages/` directory.

---

## Notes

1. **The original text cannot be modified** — Translation files use the original text as the key; modifying the original text will cause translations to fail. If you need to modify the original text, re-run `translate init` to generate a skeleton and re-translate.
2. **Character names are only replaced for display** — The logic layer (character entering/leaving/saving/jumping) always uses the original character names; translations only take effect when displayed on screen.
3. **Empty translations fall back to the original text** — When a translation is an empty string, it automatically falls back to displaying the original text, with no error.
4. **Hot reload compatible** — After modifying a translation file, it takes effect immediately via hot reload, with no need to restart the game.
5. **Save file compatibility** — Save files store the original text and section names; switching languages does not affect save file loading.
