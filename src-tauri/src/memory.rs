//! Plain-Markdown long-term memory.
//!
//! Memory lives outside SQLite as a directory of flat `.md` files in the app
//! data dir. There is no frontmatter, no categories, and no generated index:
//! a small fixed set of **core** files is injected into every system prompt in
//! full, and every other file is listed by name + first line so the model can
//! `read_memory` on demand. Live turns may `save_memory` (append to an inbox);
//! the idle reflection pass curates with `write_memory` (full-file replace).

use std::path::PathBuf;

use serde::Serialize;
use serde_json::Value;

use crate::tools::{ToolCall, ToolOutput};

/// Files injected into every system prompt in full, in display order. The system
/// starts with only these; the agent may grow it with new topic files.
pub const CORE_FILES: &[&str] = &["profile.md", "preferences.md", "goals.md"];

/// Legacy generated index; deleted on migration.
const LEGACY_INDEX_FILE: &str = "index.md";

/// One memory file, as surfaced to the Memory tab.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryFile {
    pub name: String,
    pub content: String,
    /// Core files are always injected in full and pinned first in the UI.
    pub core: bool,
}

/// Shared memory state installed into Tauri. Holds the memory directory; files
/// on disk are the source of truth.
pub struct MemoryState {
    dir: PathBuf,
}

impl MemoryState {
    /// Load (creating if needed) the memory directory at `dir`, migrating any
    /// frontmatter-formatted files from the previous memory system.
    pub fn load(dir: PathBuf) -> Result<Self, String> {
        let state = Self { dir };
        state.ensure()?;
        state.migrate_legacy()?;
        Ok(state)
    }

    fn ensure_dir(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| format!("create memory dir: {e}"))
    }

    /// Create the directory and the core files (empty when new).
    fn ensure(&self) -> Result<(), String> {
        self.ensure_dir()?;
        for file in CORE_FILES {
            let path = self.dir.join(file);
            if !path.exists() {
                std::fs::write(&path, "").map_err(|e| format!("create {file}: {e}"))?;
            }
        }
        Ok(())
    }

    fn path_for(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    /// Write a file atomically (temp + rename) so a crash can't truncate it.
    fn write_file_raw(&self, name: &str, content: &str) -> Result<(), String> {
        self.ensure_dir()?;
        let path = self.path_for(name);
        let tmp = self.dir.join(format!(".{name}.tmp"));
        std::fs::write(&tmp, content).map_err(|e| format!("write {name}: {e}"))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("write {name}: {e}"))
    }

    /// One-time conversion of legacy frontmatter entries into plain Markdown,
    /// and removal of the generated `index.md`.
    fn migrate_legacy(&self) -> Result<(), String> {
        let legacy_index = self.dir.join(LEGACY_INDEX_FILE);
        if legacy_index.exists() {
            let _ = std::fs::remove_file(&legacy_index);
        }
        let Ok(rd) = std::fs::read_dir(&self.dir) else {
            return Ok(());
        };
        for entry in rd.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if !name.ends_with(".md") || name.starts_with('.') {
                continue;
            }
            let Ok(raw) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            if let Some(converted) = legacy_to_plain(&raw) {
                self.write_file_raw(&name, &converted)?;
            }
        }
        Ok(())
    }

    /// Every `.md` file in the memory dir, core files first, then others sorted.
    pub fn file_names(&self) -> Vec<String> {
        let mut core: Vec<String> = CORE_FILES.iter().map(|f| f.to_string()).collect();
        let mut extras: Vec<String> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&self.dir) {
            for entry in rd.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    if name.ends_with(".md")
                        && !name.starts_with('.')
                        && !core.iter().any(|c| c == name)
                        && !extras.iter().any(|e| e == name)
                    {
                        extras.push(name.to_string());
                    }
                }
            }
        }
        extras.sort();
        core.extend(extras);
        core
    }

    /// Read a file's full contents (empty when it doesn't exist).
    pub fn read(&self, path: &str) -> Result<String, String> {
        let name = normalize_name(path)?;
        match std::fs::read_to_string(self.path_for(&name)) {
            Ok(text) => Ok(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(e) => Err(format!("read {name}: {e}")),
        }
    }

    /// Append a statement to the named file, creating it if needed. Returns the
    /// file name. A file name is always required (there is no default inbox).
    pub(crate) fn append(&self, content: &str, path: &str) -> Result<String, String> {
        self.ensure()?;
        let content = content.trim();
        if content.is_empty() {
            return Err("save_memory: 'content' is empty".into());
        }
        let name = normalize_name(path)?;
        let existing = self.read(&name)?;
        let mut out = existing;
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        for line in content.lines() {
            out.push_str("- ");
            out.push_str(line.trim());
            out.push('\n');
        }
        self.write_file_raw(&name, &out)?;
        Ok(name)
    }

    /// Full-file replace (curation), creating the file if needed.
    pub(crate) fn write(&self, name: &str, content: &str) -> Result<(), String> {
        let name = normalize_name(name)?;
        self.write_file_raw(&name, content)
    }

    /// The bounded block injected into every system prompt: core files in full,
    /// then a listing of the rest.
    pub fn context_block(&self) -> String {
        let mut out = String::from(
            "## Long-term memory\n\
             You have a long-term memory kept as plain Markdown files. The core files below are \
             shown in full; other files are listed and can be read on demand.\n\
             - `read_memory(path)` reads a file's full contents.\n\
             - `save_memory(content, path)` appends a statement to the named file.\n\
             - `write_memory(path, content)` replaces a file (use to merge/dedupe).\n\
             The system starts with the core files below; grow it by creating new files when a \
             subject warrants one. Route facts by topic: identity/background → profile.md, \
             preferences → preferences.md, goals → goals.md. Create a new file only for a \
             substantial, recurring subject. Keep memory to durable, user-specific facts — not \
             transient chat context.\n\
             Explicit user preferences are set by the user in Settings and are authoritative; \
             do not duplicate or contradict them here.\n",
        );
        let mut listed: Vec<String> = Vec::new();
        for name in self.file_names() {
            let content = self.read(&name).unwrap_or_default();
            let trimmed = content.trim();
            if CORE_FILES.contains(&name.as_str()) {
                out.push_str(&format!("\n### {name}\n"));
                if trimmed.is_empty() {
                    out.push_str("_(empty)_\n");
                } else {
                    out.push_str(trimmed);
                    out.push('\n');
                }
            } else {
                let first = first_line(trimmed);
                if first.is_empty() {
                    listed.push(format!("- {name}"));
                } else {
                    listed.push(format!("- {name} — {first}"));
                }
            }
        }
        if !listed.is_empty() {
            out.push_str("\nOther files:\n");
            out.push_str(&listed.join("\n"));
            out.push('\n');
        }
        out
    }

    /// All memory files for the Memory tab (core files always present).
    pub fn list_files(&self) -> Result<Vec<MemoryFile>, String> {
        self.ensure()?;
        Ok(self
            .file_names()
            .into_iter()
            .map(|name| MemoryFile {
                content: std::fs::read_to_string(self.dir.join(&name)).unwrap_or_default(),
                core: CORE_FILES.contains(&name.as_str()),
                name,
            })
            .collect())
    }

    /// Write a user-edited memory file.
    pub fn write_file(&self, name: &str, content: &str) -> Result<(), String> {
        self.write(name, content)
    }

    /// Delete a memory file.
    pub fn delete_file(&self, name: &str) -> Result<(), String> {
        let name = normalize_name(name)?;
        let path = self.path_for(&name);
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| format!("delete {name}: {e}"))?;
        }
        Ok(())
    }
}

#[tauri::command]
pub fn list_memory_files(state: tauri::State<'_, MemoryState>) -> Result<Vec<MemoryFile>, String> {
    state.list_files()
}

#[tauri::command]
pub fn write_memory_file(
    state: tauri::State<'_, MemoryState>,
    name: String,
    content: String,
) -> Result<(), String> {
    state.write_file(&name, &content)
}

#[tauri::command]
pub fn delete_memory_file(
    state: tauri::State<'_, MemoryState>,
    name: String,
) -> Result<(), String> {
    state.delete_file(&name)
}

/// Execute a memory tool call. Errors come back as tool-error text so the model
/// can self-correct (never throws), matching the host tools.
pub async fn execute_tool(call: &ToolCall, memory: &MemoryState) -> ToolOutput {
    match call.name.as_str() {
        "save_memory" => {
            let content = call.arguments.get("content").and_then(Value::as_str);
            let path = call
                .arguments
                .get("path")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|p| !p.is_empty());
            match (content, path) {
                (Some(content), Some(path)) => match memory.append(content, path) {
                    Ok(file) => ToolOutput::ok(format!("Saved to {file}.")),
                    Err(e) => ToolOutput::err(e),
                },
                (None, _) => ToolOutput::err("save_memory: missing 'content' argument".into()),
                (_, None) => ToolOutput::err(
                    "save_memory: missing 'path' argument (choose a memory file, e.g. profile.md, \
                     or create a new one)"
                        .into(),
                ),
            }
        }
        "read_memory" => match call.arguments.get("path").and_then(Value::as_str) {
            Some(path) => match memory.read(path) {
                Ok(text) if text.trim().is_empty() => ToolOutput::ok(format!("{path} is empty.")),
                Ok(text) => ToolOutput::ok(text),
                Err(e) => ToolOutput::err(e),
            },
            None => ToolOutput::err("read_memory: missing 'path' argument".into()),
        },
        "write_memory" => {
            let name = call.arguments.get("path").and_then(Value::as_str);
            let content = call.arguments.get("content").and_then(Value::as_str);
            match (name, content) {
                (Some(name), Some(content)) => match memory.write(name, content) {
                    Ok(()) => ToolOutput::ok(format!("Wrote {name}.")),
                    Err(e) => ToolOutput::err(e),
                },
                _ => ToolOutput::err("write_memory: missing 'path' or 'content' argument".into()),
            }
        }
        other => ToolOutput::err(format!("unknown memory tool: {other}")),
    }
}

/// Accept only a bare `.md` file name (optionally prefixed with `memory/`); this
/// rejects path traversal and keeps reads/writes inside the memory directory.
fn normalize_name(name: &str) -> Result<String, String> {
    let name = name.trim().trim_start_matches("./");
    let name = name.strip_prefix("memory/").unwrap_or(name);
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || !name.to_ascii_lowercase().ends_with(".md")
    {
        return Err(format!(
            "invalid memory path: '{name}' (expected a .md file name)"
        ));
    }
    Ok(name.to_ascii_lowercase())
}

/// First non-empty line, trimmed and capped — the listing hint for non-core files.
fn first_line(content: &str) -> String {
    let line = content
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let mut out: String = line.chars().take(100).collect();
    if line.chars().count() > 100 {
        out.push('…');
    }
    out
}

/// Convert a legacy frontmatter-formatted file into plain Markdown. Returns
/// `None` when the file has no frontmatter (already plain, or empty).
fn legacy_to_plain(raw: &str) -> Option<String> {
    let lines: Vec<&str> = raw.lines().collect();
    let mut out = String::new();
    let mut i = 0;
    let mut found = false;
    while i < lines.len() {
        if lines[i].trim() != "---" {
            i += 1;
            continue;
        }
        let mut title = String::new();
        let mut j = i + 1;
        while j < lines.len() && lines[j].trim() != "---" {
            if let Some((key, value)) = lines[j].split_once(':') {
                if key.trim().eq_ignore_ascii_case("title") {
                    title = unquote(value.trim());
                }
            }
            j += 1;
        }
        if j >= lines.len() {
            break; // unterminated frontmatter
        }
        let body_start = j + 1;
        let mut k = body_start;
        while k < lines.len() && lines[k].trim() != "---" {
            k += 1;
        }
        let body = lines[body_start..k].join("\n");
        if !title.is_empty() || !body.trim().is_empty() {
            found = true;
            if !out.is_empty() {
                out.push('\n');
            }
            if title.is_empty() {
                out.push_str(body.trim());
                out.push('\n');
            } else {
                out.push_str(&format!("## {title}\n\n{}\n", body.trim()));
            }
        }
        i = k;
    }
    found.then_some(out)
}

fn unquote(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"' {
        value[1..value.len() - 1]
            .replace("\\\"", "\"")
            .replace("\\\\", "\\")
    } else if bytes.len() >= 2 && bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\'' {
        value[1..value.len() - 1].to_string()
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("pi-chat-memory-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    fn state(name: &str) -> MemoryState {
        MemoryState::load(temp_dir(name)).unwrap()
    }

    #[test]
    fn load_creates_core_files() {
        let s = state("create");
        for file in CORE_FILES {
            assert!(s.dir.join(file).exists(), "{file} missing");
        }
    }

    #[test]
    fn append_requires_a_file_and_creates_it() {
        let s = state("append");
        let file = s.append("User prefers dark mode.", "profile.md").unwrap();
        assert_eq!(file, "profile.md");
        assert!(s.read("profile.md").unwrap().contains("User prefers dark mode."));
        // Appending again accumulates rather than replacing.
        s.append("Likes tea.", "profile.md").unwrap();
        let body = s.read("profile.md").unwrap();
        assert!(body.contains("dark mode"));
        assert!(body.contains("Likes tea"));
    }

    #[test]
    fn append_can_create_a_topic_file() {
        let s = state("append-core");
        s.append("Uses Rust.", "project-x.md").unwrap();
        assert!(s.read("project-x.md").unwrap().contains("Rust"));
    }

    #[test]
    fn context_block_injects_core_in_full_and_lists_others() {
        let s = state("context");
        s.write("profile.md", "Name is Ravi.").unwrap();
        s.write("project-x.md", "Project X uses Rust.\nMore detail.").unwrap();
        let block = s.context_block();
        assert!(block.contains("### profile.md"));
        assert!(block.contains("Name is Ravi."));
        // Non-core file: name + first line only, not the second line.
        assert!(block.contains("- project-x.md — Project X uses Rust."));
        assert!(!block.contains("More detail."));
        assert!(!block.contains("notes.md"));
    }

    #[test]
    fn write_replaces_and_delete_removes() {
        let s = state("write-delete");
        s.write("profile.md", "one").unwrap();
        s.write("profile.md", "two").unwrap();
        assert_eq!(s.read("profile.md").unwrap(), "two");
        s.delete_file("profile.md").unwrap();
        assert!(!s.dir.join("profile.md").exists());
    }

    #[test]
    fn list_files_marks_core_first() {
        let s = state("list");
        s.write("zebra.md", "z").unwrap();
        let files = s.list_files().unwrap();
        let names: Vec<&str> = files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(&names[..CORE_FILES.len()], CORE_FILES);
        assert!(names.contains(&"zebra.md"));
        assert!(files.iter().find(|f| f.name == "profile.md").unwrap().core);
        assert!(!files.iter().find(|f| f.name == "zebra.md").unwrap().core);
    }

    #[test]
    fn normalize_name_rejects_traversal_and_non_md() {
        assert!(normalize_name("../../etc/passwd").is_err());
        assert!(normalize_name("preferences.txt").is_err());
        assert!(normalize_name("sub/dir.md").is_err());
        assert_eq!(
            normalize_name("memory/Preferences.MD").unwrap(),
            "preferences.md"
        );
        assert_eq!(normalize_name("./notes.md").unwrap(), "notes.md");
    }

    #[test]
    fn migrates_legacy_frontmatter_to_plain_markdown() {
        let dir = temp_dir("migrate");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("preferences.md"),
            "---\ntitle: \"Dark mode\"\ncategory: preference\nimportance: 4\ncreated: 2026-09-10\nsummary: \"Likes dark.\"\n---\nUser prefers dark mode.\n",
        )
        .unwrap();
        std::fs::write(dir.join("index.md"), "# Memory index\n").unwrap();
        let s = MemoryState::load(dir).unwrap();
        let plain = s.read("preferences.md").unwrap();
        assert!(plain.contains("## Dark mode"));
        assert!(plain.contains("User prefers dark mode."));
        assert!(!plain.contains("importance"));
        assert!(!s.dir.join("index.md").exists());
    }

    #[test]
    fn legacy_to_plain_returns_none_without_frontmatter() {
        assert!(legacy_to_plain("# Just a note\n\nhello").is_none());
        assert!(legacy_to_plain("").is_none());
    }

    #[tokio::test]
    async fn execute_tool_saves_reads_and_writes() {
        let s = state("execute");
        let save = ToolCall {
            id: "t".into(),
            name: "save_memory".into(),
            arguments: serde_json::json!({"content": "User's name is Ravi.", "path": "profile.md"}),
        };
        let out = execute_tool(&save, &s).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("profile.md"));

        let read = ToolCall {
            id: "t".into(),
            name: "read_memory".into(),
            arguments: serde_json::json!({"path": "profile.md"}),
        };
        let out = execute_tool(&read, &s).await;
        assert!(!out.is_error);
        assert!(out.content.contains("User's name is Ravi."));

        let write = ToolCall {
            id: "t".into(),
            name: "write_memory".into(),
            arguments: serde_json::json!({
                "path": "profile.md",
                "content": "## Identity\n\nName is Ravi.\n"
            }),
        };
        let out = execute_tool(&write, &s).await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(s.read("profile.md").unwrap(), "## Identity\n\nName is Ravi.\n");
    }
}
