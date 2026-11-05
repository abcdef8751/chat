//! Skills: user-authored instruction bundles in `<app data>/skills/*.md`.
//!
//! A skill is a Markdown file whose stem is its id. Optional YAML frontmatter
//! (`name`, `description`) supplies the label shown in the composer's `/` menu;
//! otherwise the first `# Heading` and first paragraph are used. The body is
//! injected as a system message when the user starts a message with
//! `/skill-id`.
//!
//! Files are read on demand — there is no index to keep in sync, so dropping a
//! new Markdown file into the directory is all it takes (the Settings panel's
//! Reload button just re-lists).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::tools::{ToolCall, ToolOutput};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillInfo {
    pub id: String,
    pub name: String,
    pub description: String,
}

/// Where the skills live (set once from `setup`).
static DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

pub fn init(dir: PathBuf) {
    // A missing directory is not an error: the app creates it so the user has a
    // place to drop files. A failure is logged and listing simply stays empty.
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("skills: creating {} failed: {e}", dir.display());
    }
    *DIR.lock().unwrap() = Some(dir);
}

/// The skills directory, if `init` ran.
pub fn dir_path() -> Option<PathBuf> {
    DIR.lock().unwrap().clone()
}

/// One parsed skill file.
struct ParsedSkill {
    /// Display name: frontmatter `name`, else the first `# Heading`, else the id.
    name: String,
    /// Frontmatter `description`, else the first non-empty paragraph.
    description: String,
    /// The instructions, with frontmatter removed.
    body: String,
}

fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            let e = e.to_ascii_lowercase();
            e == "md" || e == "markdown"
        })
        .unwrap_or(false)
}

/// Split optional `---` frontmatter from the body and parse its simple
/// `key: value` pairs. Frontmatter is only recognized as the very first line
/// (a leading BOM is tolerated); anything else is ordinary Markdown.
fn split_frontmatter(raw: &str) -> (Vec<(String, String)>, String) {
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    let mut lines = raw.lines();
    let Some(first) = lines.next() else {
        return (Vec::new(), String::new());
    };
    if first.trim() != "---" {
        return (Vec::new(), raw.to_string());
    }
    let mut fields = Vec::new();
    let mut closed = false;
    for line in lines.by_ref() {
        let trimmed = line.trim();
        if trimmed == "---" || trimmed == "..." {
            closed = true;
            break;
        }
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // `key: value`; a value may contain further colons (URLs, sentences).
        if let Some((key, value)) = trimmed.split_once(':') {
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim();
            // Strip one matching pair of YAML quotes.
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .or_else(|| {
                    value
                        .strip_prefix('\'')
                        .and_then(|v| v.strip_suffix('\''))
                })
                .unwrap_or(value);
            if !key.is_empty() && !value.is_empty() {
                fields.push((key, value.to_string()));
            }
        }
    }
    if !closed {
        // An unterminated `---` block is not frontmatter; keep the file as-is.
        return (Vec::new(), raw.to_string());
    }
    let body = lines.collect::<Vec<_>>().join("\n");
    (fields, body)
}

/// Build a [`ParsedSkill`] from raw file text (frontmatter already handled by
/// [`split_frontmatter`]).
fn parse_skill(raw: &str, id: &str) -> ParsedSkill {
    let (fields, body) = split_frontmatter(raw);
    let field = |key: &str| {
        fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };

    // Fallbacks from the body: first `# Heading`, then the first paragraph.
    let mut heading: Option<String> = None;
    let mut paragraph: Option<String> = None;
    let mut paragraph_lines: Vec<String> = Vec::new();
    let mut in_paragraph = false;
    for line in body.lines() {
        let trimmed = line.trim();
        if heading.is_none() {
            if let Some(title) = trimmed.strip_prefix("# ") {
                let title = title.trim();
                if !title.is_empty() {
                    heading = Some(title.to_string());
                    continue;
                }
            }
        }
        if paragraph.is_none() {
            if trimmed.is_empty() {
                if in_paragraph {
                    paragraph = Some(paragraph_lines.join(" "));
                }
                in_paragraph = false;
                continue;
            }
            // The heading line itself is not the description paragraph.
            if heading.is_some() && trimmed.starts_with("# ") {
                continue;
            }
            in_paragraph = true;
            paragraph_lines.push(trimmed.to_string());
        }
    }
    if paragraph.is_none() && !paragraph_lines.is_empty() {
        paragraph = Some(paragraph_lines.join(" "));
    }

    let name = field("name")
        .or(heading)
        .unwrap_or_else(|| id.to_string());
    let description = field("description")
        .or(paragraph)
        .map(|d| {
            // The list is a compact hint, not the skill body.
            let mut d: String = d.chars().take(200).collect();
            if d.chars().count() >= 200 {
                d.push('…');
            }
            d
        })
        .unwrap_or_default();

    ParsedSkill {
        name,
        description,
        body: body.trim_start_matches('\n').to_string(),
    }
}

/// Find the file for `id` in `dir`, case-insensitively. Refuses path traversal:
/// an id is a file stem, never a path.
fn find_skill_path(dir: &Path, id: &str) -> Result<PathBuf, String> {
    let id = id.trim();
    if id.is_empty()
        || id.contains('/')
        || id.contains('\\')
        || id.contains("..")
        || id.contains('\0')
    {
        return Err(format!("unknown skill: {id}"));
    }
    let entries = std::fs::read_dir(dir).map_err(|e| format!("cannot read skills directory: {e}"))?;
    for entry in entries.flatten() {
        // `file_type` does not follow symlinks, so a link placed in the skills
        // directory cannot make this read outside it.
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        if !is_markdown(&path) {
            continue;
        }
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if stem.eq_ignore_ascii_case(id) {
            return Ok(path);
        }
        // The slash menu shows the display name (`# Summarize`), so accept that
        // too — while the file stem stays the canonical id.
        if let Ok(raw) = std::fs::read_to_string(&path) {
            if parse_skill(&raw, stem).name.eq_ignore_ascii_case(id) {
                return Ok(path);
            }
        }
    }
    Err(format!("unknown skill: {id}"))
}

/// A skill body is injected into every request where it is invoked; cap it so a
/// huge Markdown file cannot fail the turn or cost a fortune.
const MAX_SKILL_CHARS: usize = 100_000;

/// The skills directory, or an actionable error when it was never published.
fn skill_dir() -> Result<PathBuf, String> {
    dir_path().ok_or_else(|| "skills directory is not configured".to_string())
}

/// Validate and canonicalize a skill id.
///
/// An id is a file stem, never a path. Leading/trailing whitespace is trimmed,
/// the id is lowercased, and runs of whitespace become `-` (`Code Review` →
/// `code-review`). Anything else outside `[a-z0-9_-]` is refused with an
/// actionable message rather than silently written to a surprising filename.
fn normalize_skill_id(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("invalid skill id: it must not be empty".into());
    }
    if trimmed.contains('/') || trimmed.contains('\\') || trimmed.contains("..") {
        return Err(format!(
            "invalid skill id '{trimmed}': an id is a file name, not a path"
        ));
    }
    if trimmed.contains('\0') {
        return Err(format!("invalid skill id '{trimmed}': it contains a NUL byte"));
    }
    if trimmed.chars().count() > 64 {
        return Err(format!(
            "invalid skill id '{trimmed}': it is longer than 64 characters"
        ));
    }
    let mut out = String::with_capacity(trimmed.len());
    let mut in_space = false;
    for ch in trimmed.chars() {
        if ch.is_whitespace() {
            in_space = true;
            continue;
        }
        if in_space && !out.is_empty() {
            out.push('-');
        }
        in_space = false;
        out.extend(ch.to_lowercase());
    }
    if out.is_empty()
        || out.len() > 64
        || !out
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        return Err(format!(
            "invalid skill id '{trimmed}': use 1-64 characters of a-z, 0-9, '_' or '-' \
             (spaces become '-')"
        ));
    }
    Ok(out)
}

/// Strip a leading `---` frontmatter block, keeping the body. Only a well-formed
/// block (opened on the very first line, closed by `---`/`...`) is removed, so a
/// leading horizontal rule is not mistaken for metadata.
fn without_frontmatter(raw: &str) -> &str {
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    let mut lines = raw.split_inclusive('\n');
    let Some(first) = lines.next() else {
        return raw;
    };
    if first.trim() != "---" {
        return raw;
    }
    let mut offset = first.len();
    for line in lines {
        offset += line.len();
        let trimmed = line.trim();
        if trimmed == "---" || trimmed == "..." {
            return raw[offset..].trim_start_matches(['\r', '\n']);
        }
    }
    raw
}

/// Render one YAML scalar for frontmatter. Newlines are flattened (frontmatter
/// is line-oriented) and values that would need quoting get a matching pair.
fn yaml_scalar(value: &str) -> String {
    let value: String = value
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    let value = value.trim();
    let needs_quotes = value.contains(':')
        || value.contains('#')
        || value.starts_with(['"', '\'', '-', '?', '&', '*', '!', '|', '>', '[', '{']);
    if !needs_quotes {
        return value.to_string();
    }
    if !value.contains('"') {
        format!("\"{value}\"")
    } else if !value.contains('\'') {
        format!("'{value}'")
    } else {
        value.to_string()
    }
}

/// Build a complete skill file: optional frontmatter (only for the fields that
/// were provided) followed by the body. Any frontmatter already in `content` is
/// replaced when new fields are supplied, so re-saving a skill the model read
/// with `read_skill` never nests two blocks.
fn compose_skill(content: &str, name: Option<&str>, description: Option<&str>) -> String {
    let name = name.map(str::trim).filter(|v| !v.is_empty());
    let description = description.map(str::trim).filter(|v| !v.is_empty());
    if name.is_none() && description.is_none() {
        return content.to_string();
    }
    let mut out = String::from("---\n");
    if let Some(name) = name {
        out.push_str(&format!("name: {}\n", yaml_scalar(name)));
    }
    if let Some(description) = description {
        out.push_str(&format!("description: {}\n", yaml_scalar(description)));
    }
    out.push_str("---\n");
    let body = without_frontmatter(content);
    if !body.is_empty() && !body.starts_with('\n') {
        out.push('\n');
    }
    out.push_str(body);
    out
}

/// Create or overwrite a skill, returning the resolved id.
///
/// `id` is normalized (see [`normalize_skill_id`]); if it already resolves to a
/// file — case-insensitively by stem, or by display name through
/// [`find_skill_path`] — that exact file is overwritten, so saving through the
/// name shown in the `/` menu updates the skill instead of creating a duplicate
/// next to it.
///
/// Content over [`MAX_SKILL_CHARS`] is refused: a truncated skill would be
/// silently wrong, so the model must shorten it instead.
pub fn save(
    id: &str,
    content: &str,
    name: Option<&str>,
    description: Option<&str>,
) -> Result<String, String> {
    let chars = content.chars().count();
    if chars > MAX_SKILL_CHARS {
        return Err(format!(
            "save_skill: content is {chars} characters, over the {MAX_SKILL_CHARS}-character \
             limit; shorten it (a truncated skill would be silently wrong)"
        ));
    }
    let normalized = normalize_skill_id(id)?;
    let dir = skill_dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create skills directory: {e}"))?;
    // Reuse an existing file when the id names one: by stem (case-insensitive)
    // or by display name. The file's stem is the canonical id from then on.
    let existing = find_skill_path(&dir, &normalized).ok();
    let resolved = existing
        .as_deref()
        .and_then(|p| p.file_stem())
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| normalized.clone());
    let path = existing.unwrap_or_else(|| dir.join(format!("{resolved}.md")));
    let text = compose_skill(content, name, description);
    let tmp = dir.join(format!(".{resolved}.md.tmp"));
    std::fs::write(&tmp, &text).map_err(|e| format!("save_skill: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("save_skill: {e}"))?;
    Ok(format!(
        "Saved skill '{resolved}' ({} characters).",
        text.chars().count()
    ))
}

/// Raw Markdown of one skill **including** its frontmatter, so the model can
/// edit a skill without clobbering its metadata. A file larger than
/// [`MAX_SKILL_CHARS`] (only possible if it was copied in outside the app) is
/// truncated with an explicit marker.
pub fn read_raw(id: &str) -> Result<String, String> {
    let dir = skill_dir()?;
    let path = find_skill_path(&dir, id)?;
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("cannot read skill {id}: {e}"))?;
    if raw.chars().count() > MAX_SKILL_CHARS {
        const MARKER: &str = "\n\n…[skill truncated by pi-chat]";
        let take = MAX_SKILL_CHARS.saturating_sub(MARKER.chars().count());
        let mut capped: String = raw.chars().take(take).collect();
        capped.push_str(MARKER);
        return Ok(capped);
    }
    Ok(raw)
}

/// Delete one skill file, returning its resolved id. An unknown id is an error
/// (nothing is written or removed).
pub fn delete(id: &str) -> Result<String, String> {
    let dir = skill_dir()?;
    let path = find_skill_path(&dir, id)?;
    let resolved = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(id)
        .to_string();
    std::fs::remove_file(&path).map_err(|e| format!("delete_skill: {e}"))?;
    Ok(format!("Deleted skill '{resolved}'."))
}

/// Whether `name` is one of the skill tools (used by `chat.rs` to route
/// dispatch). They share the live/reflection/extraction tool list for prompt
/// cache alignment; access is enforced by mode at dispatch.
pub fn is_skill_tool(name: &str) -> bool {
    matches!(
        name,
        "list_skills" | "read_skill" | "save_skill" | "delete_skill"
    )
}

/// Execute a skill tool call. Errors come back as tool-error text so the model
/// can self-correct (never throws), matching the memory/host tools.
pub fn execute_tool(call: &ToolCall) -> ToolOutput {
    match call.name.as_str() {
        "list_skills" => {
            let dir = match skill_dir() {
                Ok(dir) => dir,
                Err(e) => return ToolOutput::err(e),
            };
            let skills = list();
            let mut out = format!("Skills directory: {}\n", dir.display());
            if skills.is_empty() {
                out.push_str("(no skills yet — create one with save_skill)\n");
            }
            for skill in skills {
                if skill.description.is_empty() {
                    out.push_str(&format!("{} — {}\n", skill.id, skill.name));
                } else {
                    out.push_str(&format!(
                        "{} — {}: {}\n",
                        skill.id, skill.name, skill.description
                    ));
                }
            }
            ToolOutput::ok(out)
        }
        "read_skill" => match call.arguments.get("id").and_then(Value::as_str) {
            Some(id) => match read_raw(id) {
                Ok(raw) => ToolOutput::ok(raw),
                Err(e) => ToolOutput::err(e),
            },
            None => ToolOutput::err("read_skill: missing 'id' argument".into()),
        },
        "save_skill" => {
            let id = call.arguments.get("id").and_then(Value::as_str);
            let content = call.arguments.get("content").and_then(Value::as_str);
            let name = call.arguments.get("name").and_then(Value::as_str);
            let description = call.arguments.get("description").and_then(Value::as_str);
            match (id, content) {
                (Some(id), Some(content)) => match save(id, content, name, description) {
                    Ok(resolved) => ToolOutput::ok(resolved),
                    Err(e) => ToolOutput::err(e),
                },
                (None, _) => ToolOutput::err(
                    "save_skill: missing 'id' argument (a short file-stem identifier, e.g. \
                     'code-review')"
                        .into(),
                ),
                (_, None) => ToolOutput::err("save_skill: missing 'content' argument".into()),
            }
        }
        "delete_skill" => match call.arguments.get("id").and_then(Value::as_str) {
            Some(id) => match delete(id) {
                Ok(message) => ToolOutput::ok(message),
                Err(e) => ToolOutput::err(e),
            },
            None => ToolOutput::err("delete_skill: missing 'id' argument".into()),
        },
        other => ToolOutput::err(format!("unknown skill tool: {other}")),
    }
}

/// Every readable skill, sorted by name/id.
pub fn list() -> Vec<SkillInfo> {
    let Some(dir) = dir_path() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        // Regular files only: a symlinked file could point outside the directory.
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        if !is_markdown(&path) {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        // An unreadable file is skipped rather than failing the whole list, so
        // one bad encoding cannot empty the composer's menu.
        let Ok(raw) = std::fs::read_to_string(&path) else {
            eprintln!("skills: reading {} failed", path.display());
            continue;
        };
        let parsed = parse_skill(&raw, id);
        out.push(SkillInfo {
            id: id.to_string(),
            name: parsed.name,
            description: parsed.description,
        });
    }
    out.sort_by(|a, b| {
        a.name
            .to_ascii_lowercase()
            .cmp(&b.name.to_ascii_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
    out
}

/// Re-scan the directory (the Settings panel's Reload button).
pub fn reload() -> Vec<SkillInfo> {
    list()
}

/// Raw body of one skill (without frontmatter). Errors when the id is unknown
/// or the file cannot be read.
pub fn read(id: &str) -> Result<String, String> {
    let dir = dir_path().ok_or_else(|| "skills directory is not configured".to_string())?;
    let path = find_skill_path(&dir, id)?;
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read skill {id}: {e}"))?;
    let body = parse_skill(&raw, id).body;
    if body.chars().count() > MAX_SKILL_CHARS {
        const MARKER: &str = "\n\n…[skill truncated by pi-chat]";
        let take = MAX_SKILL_CHARS.saturating_sub(MARKER.chars().count());
        let mut capped: String = body.chars().take(take).collect();
        capped.push_str(MARKER);
        return Ok(capped);
    }
    Ok(body)
}

/// If `content` starts with `/skill-id`, resolve that skill.
///
/// Returns `(user_text, skill_body)`. `user_text` has the leading `/id` token
/// removed (so a bare `/id` becomes an empty turn, which the model answers from
/// the skill instructions); `skill_body` is appended to the request as a
/// system message. Unknown slash tokens are left untouched.
pub fn expand_slash(content: &str) -> (String, Option<String>) {
    let trimmed = content.trim_start();
    let Some(rest) = trimmed.strip_prefix('/') else {
        return (content.to_string(), None);
    };
    let token_len = rest
        .char_indices()
        .find(|(_, c)| c.is_whitespace())
        .map(|(i, _)| i)
        .unwrap_or(rest.len());
    let token = &rest[..token_len];
    if token.is_empty() {
        return (content.to_string(), None);
    }
    let body = match read(token) {
        Ok(body) => body,
        Err(_) => return (content.to_string(), None),
    };
    let rest = rest[token_len..].trim_start();
    (rest.to_string(), Some(body))
}

// --- Tauri commands ---------------------------------------------------------

#[tauri::command]
pub fn list_skills() -> Result<Vec<SkillInfo>, String> {
    Ok(list())
}

#[tauri::command]
pub fn reload_skills() -> Result<Vec<SkillInfo>, String> {
    Ok(reload())
}

/// Absolute path of the skills directory (the Settings panel displays it so the
/// user can create/edit files directly).
#[tauri::command]
pub fn skills_dir() -> Result<String, String> {
    Ok(dir_path()
        .map(|p| p.display().to_string())
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `init` publishes a process-global directory, so every test that calls it
    /// must take this lock (a test could otherwise re-point the global mid-test).
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("pi-chat-skills-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn frontmatter_is_parsed_and_body_excludes_it() {
        let parsed = parse_skill(
            "---\nname: Code Review\ndescription: Review a diff carefully\n---\n\n# Review\n\nLook for bugs.\n",
            "review",
        );
        assert_eq!(parsed.name, "Code Review");
        assert_eq!(parsed.description, "Review a diff carefully");
        assert!(!parsed.body.contains("name: Code Review"));
        assert!(parsed.body.contains("Look for bugs."));

        // No frontmatter: heading + first paragraph are the fallbacks.
        let parsed = parse_skill("# Plain Skill\n\nA plain skill body.\n", "plain");
        assert_eq!(parsed.name, "Plain Skill");
        assert_eq!(parsed.description, "A plain skill body.");

        // Open quotes in YAML values are stripped.
        let parsed = parse_skill("---\nname: \"Quoted\"\n---\nbody", "q");
        assert_eq!(parsed.name, "Quoted");
    }

    #[test]
    fn slash_expansion_resolves_known_skills_only() {
        let _guard = lock();
        let dir = temp_dir("slash");
        std::fs::write(
            dir.join("summarize.md"),
            "---\nname: Summarizer\ndescription: Condense a text\n---\n\n# Summarize\nDo the thing.\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("plain.markdown"),
            "# Plain Skill\n\nA plain skill body.\n",
        )
        .unwrap();
        init(dir.clone());

        let list = list();
        assert_eq!(list.len(), 2);
        // Sorted by display name: "Plain Skill" < "Summarizer".
        assert_eq!(list[0].name, "Plain Skill");
        assert_eq!(list[1].name, "Summarizer");
        assert_eq!(list[1].id, "summarize");
        assert_eq!(list[1].description, "Condense a text");

        let body = read("summarize").unwrap();
        assert!(body.contains("Do the thing."));
        assert!(!body.contains("name: Summarizer"));

        // Path traversal is refused.
        assert!(read("../mcp").is_err());
        assert!(read("sub/dir").is_err());

        // Known slash token: case-insensitive, token removed, body returned.
        let (text, skill) = expand_slash("/summarize  please be terse");
        assert_eq!(text, "please be terse");
        assert!(skill.unwrap().contains("Do the thing."));
        let (text, skill) = expand_slash("/SUMMARIZE");
        assert_eq!(text, "");
        assert!(skill.is_some());
        // Leading whitespace before the slash is ignored too.
        let (text, skill) = expand_slash("  /plain go");
        assert_eq!(text, "go");
        assert!(skill.is_some());

        // Unknown slash token: untouched, no body.
        assert_eq!(
            expand_slash("/unknown hi"),
            ("/unknown hi".to_string(), None)
        );
        // A non-slash message is untouched.
        assert_eq!(
            expand_slash("hello /summarize"),
            ("hello /summarize".to_string(), None)
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn call(name: &str, arguments: Value) -> ToolCall {
        ToolCall {
            id: "t".into(),
            name: name.into(),
            arguments,
        }
    }

    #[test]
    fn normalize_skill_id_normalizes_and_rejects() {
        let _guard = lock();
        // Lowercase + whitespace runs become single dashes.
        assert_eq!(normalize_skill_id("Code Review").unwrap(), "code-review");
        assert_eq!(normalize_skill_id("  my   skill  ").unwrap(), "my-skill");
        assert_eq!(normalize_skill_id("already_fine-2").unwrap(), "already_fine-2");
        // Path components, traversal, NUL, and empty ids are refused.
        for bad in ["", "   ", "../mcp", "a/b", "a\\b", "..", "a\0b"] {
            assert!(
                normalize_skill_id(bad).is_err(),
                "expected {bad:?} to be rejected"
            );
        }
        // Characters outside [a-z0-9_-] survive normalization and are refused.
        assert!(normalize_skill_id("what?").is_err());
        assert!(normalize_skill_id("foo.md").is_err());
        assert!(normalize_skill_id(&"x".repeat(65)).is_err());
    }

    #[test]
    fn save_read_list_delete_roundtrip() {
        let _guard = lock();
        let dir = temp_dir("crud");
        init(dir.clone());

        let resolved = save(
            "Code Review",
            "# Review\n\nLook for bugs.\n",
            Some("Code Review"),
            Some("Review a diff carefully"),
        )
        .unwrap();
        assert!(resolved.contains("'code-review'"), "{resolved}");
        assert!(dir.join("code-review.md").exists());

        // read_raw includes frontmatter (unlike `read`).
        let raw = read_raw("code-review").unwrap();
        assert!(raw.contains("name: Code Review"), "{raw}");
        assert!(raw.contains("description: Review a diff carefully"), "{raw}");
        assert!(raw.contains("Look for bugs."));

        // Resolves by display name and case-insensitively, too.
        assert!(read_raw("CODE REVIEW").is_ok());

        let listed = list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "code-review");
        assert_eq!(listed[0].name, "Code Review");
        assert_eq!(listed[0].description, "Review a diff carefully");

        assert_eq!(delete("code-review").unwrap(), "Deleted skill 'code-review'.");
        assert!(!dir.join("code-review.md").exists());
        assert!(list().is_empty());

        // Deleting an unknown id errors.
        let err = delete("code-review").unwrap_err();
        assert!(err.contains("unknown skill"), "{err}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn frontmatter_is_written_only_for_provided_fields() {
        let _guard = lock();
        let dir = temp_dir("frontmatter");
        init(dir.clone());

        // No fields: content is stored byte-for-byte (no frontmatter invented).
        save("plain", "# Plain\n\nBody.\n", None, None).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("plain.md")).unwrap(),
            "# Plain\n\nBody.\n"
        );

        // Name only.
        save("nameonly", "Body.\n", Some("Nice Name"), None).unwrap();
        let raw = std::fs::read_to_string(dir.join("nameonly.md")).unwrap();
        assert!(raw.starts_with("---\nname: Nice Name\n---\n"), "{raw}");
        assert!(!raw.contains("description:"), "{raw}");
        assert!(raw.contains("Body."));

        // Description only.
        save("desconly", "Body.\n", None, Some("A description")).unwrap();
        let raw = std::fs::read_to_string(dir.join("desconly.md")).unwrap();
        assert!(raw.starts_with("---\ndescription: A description\n---\n"), "{raw}");
        assert!(!raw.contains("name:"), "{raw}");

        // Both, and a value containing a colon is quoted so the parser round-trips.
        save("both", "Body.\n", Some("Thing: a name"), Some("Desc")).unwrap();
        let parsed = parse_skill(
            &std::fs::read_to_string(dir.join("both.md")).unwrap(),
            "both",
        );
        assert_eq!(parsed.name, "Thing: a name");
        assert_eq!(parsed.description, "Desc");

        // Re-saving a skill read with read_skill replaces its old frontmatter
        // instead of nesting a second block.
        let raw = read_raw("both").unwrap();
        save("both", &raw, None, Some("New desc")).unwrap();
        let updated = std::fs::read_to_string(dir.join("both.md")).unwrap();
        assert_eq!(updated.matches("---").count(), 2, "{updated}");
        assert!(updated.contains("description: New desc"), "{updated}");
        assert!(!updated.contains("name:"), "{updated}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_by_display_name_overwrites_the_same_file() {
        let _guard = lock();
        let dir = temp_dir("display-name");
        init(dir.clone());

        save("summarize", "one\n", Some("Summarizer"), None).unwrap();
        // The `/` menu shows the display name; saving through it must update
        // that file, not create `summarizer.md` beside it.
        let resolved = save("Summarizer", "two\n", None, None).unwrap();
        assert!(resolved.contains("'summarize'"), "{resolved}");
        assert_eq!(list().len(), 1);
        assert!(dir.join("summarize.md").exists());
        assert!(!dir.join("summarizer.md").exists());
        assert_eq!(
            std::fs::read_to_string(dir.join("summarize.md")).unwrap(),
            "two\n"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_ids_are_rejected_without_touching_the_directory() {
        let _guard = lock();
        let dir = temp_dir("invalid");
        init(dir.clone());

        let long = "z".repeat(65);
        for bad in ["", "../escape", "sub/dir", "..", "bad id!", long.as_str()] {
            let err = save(bad, "body", None, None).unwrap_err();
            assert!(err.contains("invalid skill id"), "{bad:?}: {err}");
            assert!(read_raw(bad).is_err(), "{bad:?}");
            assert!(delete(bad).is_err(), "{bad:?}");
        }
        let entries: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
        assert!(entries.is_empty(), "no files should have been created");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn oversized_content_is_refused_and_nothing_is_written() {
        let _guard = lock();
        let dir = temp_dir("oversized");
        init(dir.clone());

        let huge = "x".repeat(MAX_SKILL_CHARS + 1);
        let err = save("huge", &huge, None, None).unwrap_err();
        assert!(err.contains("over the 100000-character limit"), "{err}");
        assert!(!dir.join("huge.md").exists());

        // Exactly at the cap is fine.
        let at_cap = "y".repeat(MAX_SKILL_CHARS);
        assert!(save("atcap", &at_cap, None, None).is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_of_unknown_id_errors_and_leaves_the_directory_untouched() {
        let _guard = lock();
        let dir = temp_dir("delete-unknown");
        init(dir.clone());
        save("keep", "body", None, None).unwrap();

        let err = delete("missing").unwrap_err();
        assert!(err.contains("unknown skill"), "{err}");
        assert!(dir.join("keep.md").exists());
        assert_eq!(list().len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn execute_tool_reports_ok_and_error_outcomes() {
        let _guard = lock();
        let dir = temp_dir("execute-tool");
        init(dir.clone());

        // Empty list: still names the directory so the model can inspect it.
        let out = execute_tool(&call("list_skills", Value::Null));
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains(&dir.display().to_string()), "{}", out.content);
        assert!(out.content.contains("no skills yet"), "{}", out.content);

        // Missing arguments are tool errors, not panics.
        let out = execute_tool(&call("read_skill", Value::Null));
        assert!(out.is_error);
        assert!(out.content.contains("missing 'id'"), "{}", out.content);
        let out = execute_tool(&call("save_skill", serde_json::json!({"id": "x"})));
        assert!(out.is_error);
        assert!(out.content.contains("missing 'content'"), "{}", out.content);

        // Save → list → read → delete through the tool surface.
        let out = execute_tool(&call(
            "save_skill",
            serde_json::json!({
                "id": "Daily Standup",
                "content": "# Standup\n\nSummarize yesterday.\n",
                "name": "Daily Standup",
                "description": "Summarize the day"
            }),
        ));
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("daily-standup"), "{}", out.content);

        let out = execute_tool(&call("list_skills", Value::Null));
        assert!(!out.is_error);
        assert!(
            out.content.contains("daily-standup — Daily Standup: Summarize the day"),
            "{}",
            out.content
        );

        let out = execute_tool(&call("read_skill", serde_json::json!({"id": "daily-standup"})));
        assert!(!out.is_error);
        assert!(out.content.contains("name: Daily Standup"), "{}", out.content);

        // Oversized content and an unknown id are clean errors.
        let out = execute_tool(&call(
            "save_skill",
            serde_json::json!({"id": "big", "content": "x".repeat(MAX_SKILL_CHARS + 1)}),
        ));
        assert!(out.is_error);
        assert!(out.content.contains("limit"), "{}", out.content);

        let out = execute_tool(&call("delete_skill", serde_json::json!({"id": "nope"})));
        assert!(out.is_error);
        assert!(out.content.contains("unknown skill"), "{}", out.content);

        let out = execute_tool(&call("delete_skill", serde_json::json!({"id": "daily-standup"})));
        assert!(!out.is_error, "{}", out.content);
        assert!(list().is_empty());

        let out = execute_tool(&call("not_a_skill_tool", Value::Null));
        assert!(out.is_error);
        assert!(out.content.contains("unknown skill tool"), "{}", out.content);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
