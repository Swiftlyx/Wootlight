//! Locations for settings, logs and the extracted SDK.

use std::{
    env,
    fs::{self, OpenOptions},
    path::PathBuf,
};

const APP_DIR: &str = "Wootlight";
const CONFIG_NAME: &str = "Wootlight.toml";
/// Settings file names of versions before the rename: next to the executable, then under %APPDATA%.
const LEGACY_CONFIG_NAME: &str = "WootingEQ.toml";
const LEGACY_APP_DIR: &str = "WootingEQ";
const LEGACY_ROAMING_NAME: &str = "config.toml";

fn exe_dir() -> Option<PathBuf> {
    env::current_exe().ok().and_then(|p| p.parent().map(PathBuf::from))
}

#[cfg(windows)]
fn env_dir(var: &str, app_dir: &str) -> PathBuf {
    env::var_os(var).map(PathBuf::from).or_else(exe_dir).unwrap_or_else(|| PathBuf::from(".")).join(app_dir)
}

/// ~/Library/Application Support/<app_dir> for both roaming and local data on macOS.
#[cfg(target_os = "macos")]
fn env_dir(_var: &str, app_dir: &str) -> PathBuf {
    env::var_os("HOME")
        .map(|home| PathBuf::from(home).join("Library/Application Support"))
        .or_else(exe_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(app_dir)
}

/// Settings next to the executable (portable). Settings of earlier versions are moved or copied
/// there once; a folder that cannot be written to (e.g. Program Files) keeps them in %APPDATA%.
#[cfg(windows)]
pub fn config_file() -> PathBuf {
    let roaming = env_dir("APPDATA", APP_DIR).join(CONFIG_NAME);
    let legacy_roaming = env_dir("APPDATA", LEGACY_APP_DIR).join(LEGACY_ROAMING_NAME);
    let Some(dir) = exe_dir() else { return roaming };
    let portable = dir.join(CONFIG_NAME);
    if portable.exists() {
        return portable;
    }
    let legacy_portable = dir.join(LEGACY_CONFIG_NAME);
    let created = if legacy_portable.exists() {
        fs::rename(&legacy_portable, &portable).is_ok()
    } else if let Some(old) = [&roaming, &legacy_roaming].into_iter().find(|p| p.exists()) {
        fs::copy(old, &portable).is_ok()
    } else {
        // An empty file loads as the defaults and is filled in on start.
        OpenOptions::new().write(true).create_new(true).open(&portable).is_ok()
    };
    if created {
        return portable;
    }
    if !roaming.exists() && legacy_roaming.exists() {
        let _ = roaming.parent().map(fs::create_dir_all);
        let _ = fs::copy(&legacy_roaming, &roaming);
    }
    roaming
}

/// On macOS the executable sits inside a signed app bundle, so settings always live in
/// ~/Library/Application Support/Wootlight; WOOTLIGHT_CONFIG points elsewhere for tests.
#[cfg(target_os = "macos")]
pub fn config_file() -> PathBuf {
    let _ = (LEGACY_CONFIG_NAME, LEGACY_APP_DIR, LEGACY_ROAMING_NAME, OpenOptions::new);
    if let Some(path) = env::var_os("WOOTLIGHT_CONFIG") {
        return PathBuf::from(path);
    }
    let path = env_dir("", APP_DIR).join(CONFIG_NAME);
    let _ = path.parent().map(fs::create_dir_all);
    path
}

/// Machine-local data: log and extracted DLL.
pub fn local_dir() -> PathBuf {
    env_dir("LOCALAPPDATA", APP_DIR)
}
