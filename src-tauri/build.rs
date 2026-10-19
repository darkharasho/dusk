fn main() {
    github_client_id();
    tauri_build::build()
}

/// Bake the GitHub OAuth client ID in at build time. CI sets it in the
/// environment; a local build can keep it in the repo's untracked `.env`.
fn github_client_id() {
    const KEY: &str = "DUSK_GITHUB_CLIENT_ID";
    println!("cargo:rerun-if-env-changed={KEY}");
    println!("cargo:rerun-if-changed=../.env");
    if std::env::var(KEY).is_ok_and(|v| !v.trim().is_empty()) {
        return;
    }
    let Ok(dotenv) = std::fs::read_to_string("../.env") else {
        return;
    };
    let value = dotenv.lines().find_map(|line| {
        let line = line.trim().strip_prefix("export ").unwrap_or(line.trim());
        let (key, value) = line.split_once('=')?;
        (key.trim() == KEY).then(|| value.trim().trim_matches(['"', '\'']).to_string())
    });
    if let Some(value) = value.filter(|v| !v.is_empty()) {
        println!("cargo:rustc-env={KEY}={value}");
    }
}
