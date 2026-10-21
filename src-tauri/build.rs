fn main() {
    github_client_id();
    updater_config();
    tauri_build::build()
}

/// Hand the updater's public key and endpoint to the code that updates a
/// `.deb`.
///
/// The updater plugin reads both from `tauri.conf.json` and cannot install a
/// `.deb`, so Dusk does that part itself — from the *same* two values rather
/// than a second copy of them. A key that drifted from the plugin's would
/// reject every update it was given, and only on the one install format
/// nobody builds on.
fn updater_config() {
    println!("cargo:rerun-if-changed=tauri.conf.json");
    let Ok(raw) = std::fs::read_to_string("tauri.conf.json") else {
        return;
    };
    let Ok(config) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return;
    };
    let updater = &config["plugins"]["updater"];
    if let Some(key) = updater["pubkey"].as_str() {
        println!("cargo:rustc-env=DUSK_UPDATER_PUBKEY={key}");
    }
    // The first endpoint, which is the only one there is. A second would be
    // a mirror of the same manifest.
    if let Some(endpoint) = updater["endpoints"][0].as_str() {
        println!("cargo:rustc-env=DUSK_UPDATER_ENDPOINT={endpoint}");
    }
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
