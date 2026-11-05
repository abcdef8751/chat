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
}
