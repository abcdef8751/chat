//! Markdown-file memory.
//!
//! Memory lives outside SQLite as a directory of Markdown files in the app data
//! dir. Entries carry YAML frontmatter (title/category/importance/created/
//! summary/…); `save_memory` routes a category to its canonical file and
//! merge-writes, then regenerates `index.md`. The always-injected thing is only
//! that title+summary index — bodies are read on demand via `read_memory`.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Serialize;
use serde_json::Value;

use crate::tools::{ToolCall, ToolOutput};

/// Canonical category → file mapping. Content files in display order.
const CATEGORY_FILES: &[(&str, &str)] = &[
    ("preference", "preferences.md"),
    ("identity", "identity.md"),
    ("goal", "goals.md"),
    ("note", "notes.md"),
];

/// Auto-generated index; the model never edits it directly.
const INDEX_FILE: &str = "index.md";

const MAX_TITLE_CHARS: usize = 60;
const MAX_SUMMARY_CHARS: usize = 160;

/// Canonical file for a category, if the category is recognized.
fn canonical_file(category: &str) -> Option<&'static str> {
    CATEGORY_FILES
        .iter()
        .find(|(c, _)| *c == category)
        .map(|(_, f)| *f)
}

/// Best-effort category for a file name (defaults to `note` for custom files).
fn infer_category(file: &str) -> &'static str {
    CATEGORY_FILES
        .iter()
        .find(|(_, f)| *f == file)
        .map(|(c, _)| *c)
        .unwrap_or("note")
}

/// Shared memory state installed into Tauri. Holds the memory directory; files
/// on disk are the source of truth.
pub struct MemoryState {
    dir: PathBuf,
}

/// One memory file, as surfaced to the Memory tab.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryFile {
    pub name: String,
    pub content: String,
    /// `index.md` is generated from the entry files and is not directly editable.
    pub generated: bool,
}

/// A parsed entry (frontmatter + body).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MemoryEntry {
    title: String,
    category: String,
    importance: u8,
    created: String,
    source_conversation: Option<String>,
    summary: String,
    body: String,
}

impl MemoryState {
    /// Load (creating if needed) the memory directory at `dir`.
    pub fn load(dir: PathBuf) -> Result<Self, String> {
        let state = Self { dir };
        state.ensure()?;
        Ok(state)
    }

    fn ensure_dir(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| format!("create memory dir: {e}"))
    }

    /// Create the directory, the canonical content files, and `index.md`.
    fn ensure(&self) -> Result<(), String> {
        self.ensure_dir()?;
        for (_, file) in CATEGORY_FILES {
            let path = self.dir.join(file);
            if !path.exists() {
                std::fs::write(&path, "").map_err(|e| format!("create {file}: {e}"))?;
            }
        }
        if !self.dir.join(INDEX_FILE).exists() {
            self.regenerate_index()?;
        }
        Ok(())
    }

    fn path_for(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    fn read_file(&self, name: &str) -> Result<String, String> {
        let name = normalize_name(name)?;
        std::fs::read_to_string(self.path_for(&name)).map_err(|e| format!("read {name}: {e}"))
    }

    /// Write a file atomically (temp + rename) so a crash can't truncate it.
    fn write_file_raw(&self, name: &str, content: &str) -> Result<(), String> {
        self.ensure_dir()?;
        let path = self.path_for(name);
        let tmp = self.dir.join(format!(".{name}.tmp"));
        std::fs::write(&tmp, content).map_err(|e| format!("write {name}: {e}"))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("write {name}: {e}"))
    }

    fn read_entries(&self, file: &str) -> Result<Vec<MemoryEntry>, String> {
        let path = self.dir.join(file);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let text = std::fs::read_to_string(&path).map_err(|e| format!("read {file}: {e}"))?;
        Ok(parse_entries(&text))
    }

    fn write_entries(&self, file: &str, entries: &[MemoryEntry]) -> Result<(), String> {
        self.write_file_raw(file, &serialize_entries(entries))
    }

    /// Every memory file that should appear in the index: the canonical files
    /// first (in a stable order), then any extra `.md` files the model created.
    fn indexable_files(&self) -> Vec<String> {
        let mut files: Vec<String> = CATEGORY_FILES.iter().map(|(_, f)| f.to_string()).collect();
        if let Ok(rd) = std::fs::read_dir(&self.dir) {
            let mut extras: Vec<String> = rd
                .flatten()
                .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
                .filter(|name| {
                    name.ends_with(".md") && name != INDEX_FILE && !files.contains(name)
                })
                .collect();
            extras.sort();
            files.extend(extras);
        }
        files
    }

    /// Regenerate `index.md` from the title+summary of every entry.
    fn regenerate_index(&self) -> Result<(), String> {
        let mut out = String::from("# Memory index\n");
        let mut any = false;
        for file in self.indexable_files() {
            let entries = self.read_entries(&file)?;
            if entries.is_empty() {
                continue;
            }
            any = true;
            out.push_str(&format!("\n## {file}\n"));
            for e in entries {
                if e.summary.is_empty() {
                    out.push_str(&format!("- {}\n", e.title));
                } else {
                    out.push_str(&format!("- {} — {}\n", e.title, e.summary));
                }
            }
        }
        if !any {
            out.push_str("\n_No memories saved yet._\n");
        }
        self.write_file_raw(INDEX_FILE, &out)
    }

    /// The bounded block injected into every system prompt.
    pub fn context_block(&self) -> String {
        let index = match self.read_file(INDEX_FILE) {
            Ok(s) => s,
            Err(_) => return String::new(),
        };
        format!(
            "## Long-term memory\n\
             You have a long-term memory kept as Markdown files. The index below lists saved \
             entries with one-line summaries.\n\
             - Use `read_memory(path)` to read a file's full contents on demand.\n\
             - Use `save_memory(content, path, category, importance)` to store a stable, \
             user-specific fact. Pass `path` to write to a specific file (created if missing), \
             or omit it and let `category` (preference | identity | goal | note) pick the file. \
             importance is 0-5. Only save durable facts, not transient chat context.\n\n{}",
            index.trim()
        )
    }

    /// Save (or update) an entry. An explicit `path` selects the target file
    /// (created if missing); otherwise `category` routes to its canonical file.
    /// Returns the target file name and the stored entry.
    pub(crate) fn save_entry(
        &self,
        content: &str,
        path: Option<&str>,
        category: Option<&str>,
        importance: i64,
        source_conversation: &str,
    ) -> Result<(String, MemoryEntry), String> {
        self.ensure()?;
        let content = content.trim();
        if content.is_empty() {
            return Err("save_memory: 'content' is empty".into());
        }
        let explicit = path.map(str::trim).filter(|p| !p.is_empty());
        let category = category
            .map(|c| c.trim().to_ascii_lowercase())
            .filter(|c| !c.is_empty());

        // An explicit path wins; otherwise route the category to its file.
        let file = match explicit {
            Some(path) => normalize_name(path)?,
            None => {
                let cat = category
                    .clone()
                    .ok_or_else(|| "save_memory: provide either 'path' or 'category'".to_string())?;
                canonical_file(&cat).map(str::to_string).ok_or_else(|| {
                    format!("save_memory: unknown category '{cat}' (use preference, identity, goal, or note)")
                })?
            }
        };
        let category = category.unwrap_or_else(|| infer_category(&file).to_string());

        let mut entries = self.read_entries(&file)?;
        let title = derive_title(content);
        let existing = entries
            .iter()
            .position(|e| e.title.eq_ignore_ascii_case(&title));

        let entry = match existing {
            Some(i) => {
                let prior = entries[i].clone();
                MemoryEntry {
                    // Keep the original title casing on an update.
                    title: prior.title,
                    category,
                    importance: importance.clamp(0, 5) as u8,
                    created: prior.created,
                    source_conversation: Some(source_conversation.to_string()),
                    summary: derive_summary(content),
                    body: content.to_string(),
                }
            }
            None => MemoryEntry {
                title,
                category,
                importance: importance.clamp(0, 5) as u8,
                created: today_utc(),
                source_conversation: Some(source_conversation.to_string()),
                summary: derive_summary(content),
                body: content.to_string(),
            },
        };

        match existing {
            Some(i) => entries[i] = entry.clone(),
            None => entries.push(entry.clone()),
        }
        self.write_entries(&file, &entries)?;
        self.regenerate_index()?;
        Ok((file, entry))
    }

    /// Body text of a memory file (frontmatter stripped for entry files).
    pub fn read_body(&self, path: &str) -> Result<String, String> {
        let name = normalize_name(path)?;
        let raw = self.read_file(&name)?;
        let entries = parse_entries(&raw);
        if entries.is_empty() {
            return Ok(raw.trim().to_string());
        }
        let bodies: Vec<String> = entries
            .into_iter()
            .filter(|e| !e.body.trim().is_empty())
            .map(|e| e.body.trim().to_string())
            .collect();
        Ok(bodies.join("\n\n---\n\n"))
    }

    /// All memory files for the Memory tab (canonical files always present).
    pub fn list_files(&self) -> Result<Vec<MemoryFile>, String> {
        self.ensure()?;
        let mut names: Vec<String> = CATEGORY_FILES.iter().map(|(_, f)| f.to_string()).collect();
        names.push(INDEX_FILE.to_string());
        if let Ok(rd) = std::fs::read_dir(&self.dir) {
            for entry in rd.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    if name.ends_with(".md") && !names.iter().any(|n| n == name) {
                        names.push(name.to_string());
                    }
                }
            }
        }
        names.sort();
        Ok(names
            .into_iter()
            .map(|name| MemoryFile {
                content: std::fs::read_to_string(self.dir.join(&name)).unwrap_or_default(),
                generated: name == INDEX_FILE,
                name,
            })
            .collect())
    }

    /// Write a user-edited memory file and refresh the index.
    pub fn write_file(&self, name: &str, content: &str) -> Result<(), String> {
        let name = normalize_name(name)?;
        if name == INDEX_FILE {
            return Err("index.md is generated automatically and cannot be edited".into());
        }
        self.write_file_raw(&name, content)?;
        self.regenerate_index()
    }

    /// Delete a memory file and refresh the index.
    pub fn delete_file(&self, name: &str) -> Result<(), String> {
        let name = normalize_name(name)?;
        if name == INDEX_FILE {
            return Err("index.md is generated automatically and cannot be deleted".into());
        }
        let path = self.path_for(&name);
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| format!("delete {name}: {e}"))?;
        }
        self.regenerate_index()
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
pub async fn execute_tool(call: &ToolCall, source: &str, memory: &MemoryState) -> ToolOutput {
    match call.name.as_str() {
        "save_memory" => {
            let content = call.arguments.get("content").and_then(Value::as_str);
            let path = call.arguments.get("path").and_then(Value::as_str);
            let category = call.arguments.get("category").and_then(Value::as_str);
            let importance = call
                .arguments
                .get("importance")
                .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
                .unwrap_or(3);
            match content {
                Some(content) => {
                    match memory.save_entry(content, path, category, importance, source) {
                        Ok((file, entry)) => ToolOutput::ok(format!(
                            "Saved to {file} (importance {}): {}",
                            entry.importance, entry.title
                        )),
                        Err(e) => ToolOutput::err(e),
                    }
                }
                None => ToolOutput::err("save_memory: missing 'content' argument".into()),
            }
        }
        "read_memory" => match call.arguments.get("path").and_then(Value::as_str) {
            Some(path) => match memory.read_body(path) {
                Ok(text) if text.is_empty() => {
                    ToolOutput::ok(format!("{path} is empty."))
                }
                Ok(text) => ToolOutput::ok(text),
                Err(e) => ToolOutput::err(e),
            },
            None => ToolOutput::err("read_memory: missing 'path' argument".into()),
        },
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
        return Err(format!("invalid memory path: '{name}' (expected a .md file name)"));
    }
    Ok(name.to_ascii_lowercase())
}

/// First line (or first sentence) of the content, trimmed to a title.
fn derive_title(content: &str) -> String {
    let first = content
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let stripped = first.trim_start_matches(['-', '*', '#', '>', ' ']);
    let end = stripped.find(['.', '!', '?', ';']).filter(|p| *p > 0);
    let base = end.map(|p| &stripped[..p]).unwrap_or(stripped).trim();
    let mut title: String = base.chars().take(MAX_TITLE_CHARS).collect();
    if base.chars().count() > MAX_TITLE_CHARS {
        title.push('…');
    }
    if title.is_empty() {
        "Memory".to_string()
    } else {
        title
    }
}

/// One-line summary: the first sentence, capped.
fn derive_summary(content: &str) -> String {
    let flat = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return String::new();
    }
    let end = flat.find(['.', '!', '?']).map(|p| p + 1).unwrap_or(flat.len());
    let cap = end.min(MAX_SUMMARY_CHARS);
    let mut summary: String = flat.chars().take(cap).collect();
    if flat.chars().count() > cap {
        summary.push('…');
    }
    summary.trim().to_string()
}

fn serialize_entries(entries: &[MemoryEntry]) -> String {
    let mut out = String::new();
    for (i, e) in entries.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str("---\n");
        out.push_str(&format!("title: {}\n", yaml_quote(&e.title)));
        out.push_str(&format!("category: {}\n", e.category));
        out.push_str(&format!("importance: {}\n", e.importance));
        out.push_str(&format!("created: {}\n", e.created));
        if let Some(source) = &e.source_conversation {
            out.push_str(&format!("source_conversation: {}\n", yaml_quote(source)));
        }
        out.push_str(&format!("summary: {}\n", yaml_quote(&e.summary)));
        out.push_str("---\n");
        out.push_str(e.body.trim());
        out.push('\n');
    }
    out
}

/// Parse one or more frontmatter-delimited entries. A standalone `---` line
/// closes the frontmatter or starts the next entry; body text must therefore
/// not contain a bare `---` line.
fn parse_entries(content: &str) -> Vec<MemoryEntry> {
    let lines: Vec<&str> = content.lines().collect();
    let mut entries = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim() != "---" {
            i += 1;
            continue;
        }
        let mut fm: HashMap<String, String> = HashMap::new();
        let mut j = i + 1;
        while j < lines.len() && lines[j].trim() != "---" {
            if let Some((key, value)) = lines[j].split_once(':') {
                fm.insert(key.trim().to_ascii_lowercase(), unquote(value.trim()));
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
        if !fm.is_empty() {
            entries.push(MemoryEntry {
                title: fm.remove("title").unwrap_or_default(),
                category: fm.remove("category").unwrap_or_default(),
                importance: fm
                    .remove("importance")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0),
                created: fm.remove("created").unwrap_or_default(),
                source_conversation: fm.remove("source_conversation"),
                summary: fm.remove("summary").unwrap_or_default(),
                body: lines[body_start..k].join("\n").trim().to_string(),
            });
        }
        i = k;
    }
    entries
}

fn yaml_quote(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
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

/// Current UTC date as `YYYY-MM-DD` (no chrono dependency).
fn today_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, m, d) = civil_from_days((secs / 86_400) as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Howard Hinnant's days-from-civil, inverted (days since 1970-01-01 → y/m/d).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
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
    fn load_creates_canonical_files_and_index() {
        let s = state("create");
        for (_, file) in CATEGORY_FILES {
            assert!(s.dir.join(file).exists(), "{file} missing");
        }
        let index = s.read_file(INDEX_FILE).unwrap();
        assert!(index.contains("No memories saved yet"));
    }

    #[test]
    fn save_entry_writes_frontmatter_and_regenerates_index() {
        let s = state("save");
        let (file, entry) = s
            .save_entry(
                "User prefers dark mode. Dislikes bright themes.",
                None,
                Some("preference"),
                4,
                "conv_1",
            )
            .unwrap();
        assert_eq!(file, "preferences.md");
        assert_eq!(entry.category, "preference");
        assert_eq!(entry.importance, 4);
        assert_eq!(entry.source_conversation.as_deref(), Some("conv_1"));

        let raw = s.read_file("preferences.md").unwrap();
        assert!(raw.contains("title: \""));
        assert!(raw.contains("category: preference"));
        assert!(raw.contains("importance: 4"));
        assert!(raw.contains("source_conversation: \"conv_1\""));
        assert!(raw.contains(&entry.body));

        let index = s.read_file(INDEX_FILE).unwrap();
        assert!(index.contains("preferences.md"));
        assert!(index.contains(&entry.title));
        assert!(index.contains(&entry.summary));
    }

    #[test]
    fn save_entry_updates_existing_title_instead_of_duplicating() {
        let s = state("dedupe");
        s.save_entry("Likes tea.", None, Some("preference"), 1, "c1")
            .unwrap();
        s.save_entry("Likes tea. Also likes coffee.", None, Some("preference"), 5, "c2")
            .unwrap();
        let entries = s.read_entries("preferences.md").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].importance, 5);
        assert_eq!(entries[0].source_conversation.as_deref(), Some("c2"));
        assert!(entries[0].body.contains("coffee"));
    }

    #[test]
    fn save_entry_can_target_a_custom_or_existing_file() {
        let s = state("custompath");
        let (file, entry) = s
            .save_entry("Project X uses Rust.", Some("project-x.md"), None, 2, "c")
            .unwrap();
        assert_eq!(file, "project-x.md");
        assert_eq!(entry.category, "note"); // inferred for a non-canonical file
        assert!(s.dir.join("project-x.md").exists());

        // The custom file appears in the index and is readable on demand.
        let index = s.read_file(INDEX_FILE).unwrap();
        assert!(index.contains("project-x.md"));
        assert!(index.contains(&entry.title));
        assert!(s.read_body("project-x.md").unwrap().contains("Rust"));

        // Writing the same fact to the existing file updates in place.
        let (file2, _) = s
            .save_entry("Project X uses Rust.", Some("project-x.md"), None, 3, "c2")
            .unwrap();
        assert_eq!(file2, "project-x.md");
        let entries = s.read_entries("project-x.md").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].importance, 3);

        // A path overrides the category mapping.
        let (file3, _) = s
            .save_entry("Actually likes light mode.", Some("project-x.md"), Some("preference"), 4, "c3")
            .unwrap();
        assert_eq!(file3, "project-x.md");
        assert!(s.read_entries("preferences.md").unwrap().is_empty());
    }

    #[test]
    fn categories_route_to_canonical_files() {
        let s = state("categories");
        s.save_entry("Name is Ravi", None, Some("identity"), 3, "c")
            .unwrap();
        s.save_entry("Ship v1", None, Some("goal"), 2, "c")
            .unwrap();
        s.save_entry("A random note", None, Some("note"), 0, "c")
            .unwrap();
        assert!(!s.read_entries("identity.md").unwrap().is_empty());
        assert!(!s.read_entries("goals.md").unwrap().is_empty());
        assert!(!s.read_entries("notes.md").unwrap().is_empty());
        assert!(s.read_entries("preferences.md").unwrap().is_empty());
        assert!(s.save_entry("x", None, Some("bogus"), 3, "c").is_err());
        assert!(s.save_entry("x", None, None, 3, "c").is_err());
    }

    #[test]
    fn read_body_strips_frontmatter() {
        let s = state("readbody");
        s.save_entry("Prefers dark mode.", None, Some("preference"), 3, "c")
            .unwrap();
        let body = s.read_body("preferences.md").unwrap();
        assert_eq!(body, "Prefers dark mode.");
        assert!(!body.contains("---"));
    }

    #[test]
    fn edit_and_delete_refresh_index() {
        let s = state("editdelete");
        s.save_entry("Prefers dark mode.", None, Some("preference"), 3, "c")
            .unwrap();
        s.write_file("preferences.md", "").unwrap();
        assert!(s.read_file(INDEX_FILE).unwrap().contains("No memories"));
        assert!(s.write_file(INDEX_FILE, "x").is_err());
        assert!(s.delete_file(INDEX_FILE).is_err());
        s.delete_file("preferences.md").unwrap();
        assert!(!s.dir.join("preferences.md").exists());
    }

    #[test]
    fn normalize_name_rejects_traversal_and_non_md() {
        assert!(normalize_name("../../etc/passwd").is_err());
        assert!(normalize_name("preferences.txt").is_err());
        assert!(normalize_name("sub/dir.md").is_err());
        assert_eq!(normalize_name("memory/Preferences.MD").unwrap(), "preferences.md");
        assert_eq!(normalize_name("./notes.md").unwrap(), "notes.md");
    }

    #[test]
    fn entries_roundtrip_through_serialize_parse() {
        let entry = MemoryEntry {
            title: "A \"quoted\" title".into(),
            category: "note".into(),
            importance: 2,
            created: "2026-09-10".into(),
            source_conversation: Some("conv_x".into()),
            summary: "One line.".into(),
            body: "line one\nline two".into(),
        };
        let text = serialize_entries(std::slice::from_ref(&entry));
        let parsed = parse_entries(&text);
        assert_eq!(parsed, vec![entry]);
    }

    #[test]
    fn today_is_well_formed() {
        let today = today_utc();
        assert_eq!(today.len(), 10);
        assert_eq!(&today[4..5], "-");
        assert_eq!(&today[7..8], "-");
    }

    #[tokio::test]
    async fn execute_tool_saves_and_reads() {
        let s = state("execute");
        let call = ToolCall {
            id: "t".into(),
            name: "save_memory".into(),
            arguments: serde_json::json!({
                "content": "User's name is Ravi.",
                "category": "identity",
                "importance": 5
            }),
        };
        let out = execute_tool(&call, "conv_9", &s).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("identity.md"));

        let read = ToolCall {
            id: "t".into(),
            name: "read_memory".into(),
            arguments: serde_json::json!({"path": "identity.md"}),
        };
        let out = execute_tool(&read, "conv_9", &s).await;
        assert!(!out.is_error);
        assert!(out.content.contains("User's name is Ravi."));

        // The model may pick the file explicitly (no category needed).
        let by_path = ToolCall {
            id: "t".into(),
            name: "save_memory".into(),
            arguments: serde_json::json!({
                "content": "Pinned to the tools project.",
                "path": "tools-project.md",
                "importance": 2
            }),
        };
        let out = execute_tool(&by_path, "conv_9", &s).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("tools-project.md"));
        assert!(s.read_body("tools-project.md").unwrap().contains("tools project"));
    }
}
