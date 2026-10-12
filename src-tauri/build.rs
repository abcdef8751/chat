use std::path::Path;

/// Embed only the publishable (anon) Supabase URL + key into the binary.
///
/// The secret/service-role key lives in `.env` for dev/admin tooling only and is
/// deliberately NOT compiled in. `SUPABASE_URL` and `SUPABASE_PUBLISHABLE_KEY`
/// are read from the `.env` file at the repo root (or the ambient environment),
/// then exposed to the crate via `env!("SUPABASE_URL")` / `option_env!`.
const ENV_VARS: &[&str] = &["SUPABASE_URL", "SUPABASE_PUBLISHABLE_KEY"];

fn main() {
    // The worktree root `.env` sits one level above `src-tauri/`.
    let env_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(".env");

    // `.env` lives *outside* the Cargo package, so without this directive cargo
    // has no reason to re-run the script when it appears or changes — it will
    // happily reuse a build-script output produced when `.env` was absent (e.g.
    // a `CARGO_TARGET_DIR` shared with a git worktree under /tmp), and the
    // binary silently keeps the `localhost.supabase.co` fallback in `sync.rs`.
    // Declaring it here makes an `.env` edit (or creation) force a rebuild.
    println!("cargo:rerun-if-changed={}", env_path.display());

    let mut found: Vec<String> = Vec::new();
    match std::fs::read_to_string(&env_path) {
        Ok(text) => {
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let Some((key, value)) = line.split_once('=') else { continue };
                let key = key.trim();
                let value = value.trim().trim_matches('"').trim_matches('\'');
                if ENV_VARS.contains(&key) && !value.is_empty() {
                    println!("cargo:rustc-env={key}={value}");
                    found.push(key.to_string());
                }
            }
        }
        Err(e) => println!(
            "cargo:warning=could not read {} ({e}); Supabase sync will be unconfigured",
            env_path.display()
        ),
    }
    for key in ENV_VARS {
        if !found.iter().any(|k| k.as_str() == *key) {
            println!(
                "cargo:warning={key} is not set in {} — cloud sync will be disabled in this build (populate .env and rebuild)",
                env_path.display()
            );
        }
    }

    tauri_build::build()
}
