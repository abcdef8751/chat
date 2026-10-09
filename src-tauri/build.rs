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

    if let Ok(text) = std::fs::read_to_string(&env_path) {
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            let key = key.trim();
            let value = value.trim().trim_matches('"').trim_matches('\'');
            if ENV_VARS.contains(&key) {
                println!("cargo:rustc-env={key}={value}");
            }
        }
    }

    tauri_build::build()
}
