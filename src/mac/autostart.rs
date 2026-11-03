//! Start at login. From the app bundle this is the app's own login item (`SMAppService
//! mainAppService`, listed under System Settings > General > Login Items); a bare binary, as
//! during development, gets a LaunchAgent in ~/Library/LaunchAgents instead.
//!
//! macOS may ask the user to approve a new login item; until then it counts as enabled here
//! and System Settings is opened at the Login Items page.

use std::{
    env, fs,
    path::{Path, PathBuf},
};

use objc2_foundation::NSBundle;
use objc2_service_management::{SMAppService, SMAppServiceStatus};

use crate::{log, paths};

const LABEL: &str = "io.github.swiftlyx.wootlight";
/// Bundle path the login item was registered from, to notice when the app has moved.
const RECORD_NAME: &str = "login-item-path";

/// Path of the running app bundle, if this process runs from one.
fn bundle_path() -> Option<PathBuf> {
    let bundle = NSBundle::mainBundle();
    bundle.bundleIdentifier()?;
    let path = PathBuf::from(bundle.bundlePath().to_string());
    path.extension().is_some_and(|e| e == "app").then_some(path)
}

fn status() -> SMAppServiceStatus {
    // SAFETY: plain query on the main app's service.
    unsafe { SMAppService::mainAppService().status() }
}

fn status_on(status: SMAppServiceStatus) -> bool {
    status == SMAppServiceStatus::Enabled || status == SMAppServiceStatus::RequiresApproval
}

/// Registers or unregisters the app's login item.
fn set_login_item(enabled: bool) -> bool {
    // SAFETY: the main app's service of this process; no arguments.
    let service = unsafe { SMAppService::mainAppService() };
    // SAFETY: plain calls on that service; errors come back as NSError.
    let result = unsafe {
        if enabled {
            service.registerAndReturnError()
        } else if status_on(service.status()) {
            service.unregisterAndReturnError()
        } else {
            Ok(())
        }
    };
    if let Err(e) = result {
        log!("autostart: login item {} failed: {}", if enabled { "registration" } else { "removal" }, e.localizedDescription());
        return false;
    }
    // SAFETY: plain query on that service.
    if enabled && unsafe { service.status() } == SMAppServiceStatus::RequiresApproval {
        log!("autostart: the login item waits for approval in System Settings");
        // SAFETY: opens System Settings; no arguments.
        unsafe { SMAppService::openSystemSettingsLoginItems() };
    }
    true
}

fn launch_agents_dir() -> Option<PathBuf> {
    env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/LaunchAgents"))
}

fn agent_file(dir: &Path, label: &str) -> PathBuf {
    dir.join(format!("{label}.plist"))
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

/// LaunchAgent that runs `exe` once when the user logs in.
fn agent_plist(label: &str, exe: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{}</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>ProcessType</key>
	<string>Interactive</string>
	<key>LimitLoadToSessionType</key>
	<string>Aqua</string>
</dict>
</plist>
"#,
        xml_escape(label),
        xml_escape(&exe.display().to_string())
    )
}

fn write_agent(dir: &Path, label: &str, exe: &Path) -> bool {
    let file = agent_file(dir, label);
    let result = fs::create_dir_all(dir).and_then(|_| fs::write(&file, agent_plist(label, exe)));
    if let Err(e) = &result {
        log!("autostart: cannot write {}: {e}", file.display());
    }
    result.is_ok()
}

fn remove_agent(dir: &Path, label: &str) -> bool {
    let file = agent_file(dir, label);
    match fs::remove_file(&file) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(e) => {
            log!("autostart: cannot remove {}: {e}", file.display());
            false
        }
    }
}

/// Whether the LaunchAgent exists and runs `exe`.
fn agent_runs(dir: &Path, label: &str, exe: &Path) -> Option<bool> {
    let text = fs::read_to_string(agent_file(dir, label)).ok()?;
    Some(text.contains(&format!("<string>{}</string>", xml_escape(&exe.display().to_string()))))
}

fn record_file() -> PathBuf {
    paths::local_dir().join(RECORD_NAME)
}

fn record(bundle: &Path) {
    let file = record_file();
    let _ = file.parent().map(fs::create_dir_all);
    let _ = fs::write(file, bundle.display().to_string());
}

pub fn is_enabled() -> bool {
    match bundle_path() {
        Some(_) => status_on(status()),
        None => launch_agents_dir().is_some_and(|dir| agent_file(&dir, LABEL).exists()),
    }
}

pub fn set_enabled(enabled: bool) -> bool {
    if let Some(bundle) = bundle_path() {
        let ok = set_login_item(enabled);
        if ok && enabled {
            record(&bundle);
        }
        // A LaunchAgent from a development build would start a second copy.
        if ok && let Some(dir) = launch_agents_dir() {
            remove_agent(&dir, LABEL);
        }
        return ok;
    }
    let (Some(dir), Ok(exe)) = (launch_agents_dir(), env::current_exe()) else { return false };
    if enabled { write_agent(&dir, LABEL, &exe) } else { remove_agent(&dir, LABEL) }
}

/// Points the login item at the current app, e.g. after it was moved, and moves a LaunchAgent
/// of a bare binary to the login item once the app runs from its bundle.
pub fn refresh() {
    let agents = launch_agents_dir();
    if let Some(bundle) = bundle_path() {
        let agent = agents.as_ref().map(|dir| agent_file(dir, LABEL)).filter(|f| f.exists());
        let on = status_on(status());
        let recorded = fs::read_to_string(record_file()).ok();
        if let Some(agent) = agent {
            if set_login_item(true) {
                record(&bundle);
                let _ = fs::remove_file(&agent);
                log!("autostart: login item created from the LaunchAgent");
            }
        } else if on && recorded.as_deref().is_some_and(|r| Path::new(r) != bundle) {
            // Registered from another place: register again so login opens this copy.
            if set_login_item(false) && set_login_item(true) {
                log!("autostart: login item updated");
            }
            record(&bundle);
        } else if on && recorded.is_none() {
            record(&bundle);
        }
        return;
    }
    let (Some(dir), Ok(exe)) = (agents, env::current_exe()) else { return };
    if agent_runs(&dir, LABEL, &exe) == Some(false) && write_agent(&dir, LABEL, &exe) {
        log!("autostart: LaunchAgent updated");
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    const TEST_LABEL: &str = "io.github.swiftlyx.wootlight.test";
    const CHILD: &str = "WOOTLIGHT_AUTOSTART_CHILD";

    #[test]
    fn plist_escapes() {
        let plist = agent_plist(LABEL, Path::new("/Apps/Tom & Jerry/Wootlight"));
        assert!(plist.contains("<string>/Apps/Tom &amp; Jerry/Wootlight</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>\n\t<true/>"));
    }

    #[test]
    fn agent_round_trip() {
        let dir = env::temp_dir().join(format!("wootlight-agents-{}", std::process::id()));
        let exe = Path::new("/Applications/Wootlight.app/Contents/MacOS/Wootlight");
        assert_eq!(agent_runs(&dir, TEST_LABEL, exe), None);
        assert!(write_agent(&dir, TEST_LABEL, exe));
        assert_eq!(agent_runs(&dir, TEST_LABEL, exe), Some(true));
        assert_eq!(agent_runs(&dir, TEST_LABEL, Path::new("/elsewhere/Wootlight")), Some(false));
        assert!(remove_agent(&dir, TEST_LABEL));
        assert!(!agent_file(&dir, TEST_LABEL).exists());
        assert!(remove_agent(&dir, TEST_LABEL));
        let _ = fs::remove_dir_all(&dir);
    }

    /// Writes and removes a real LaunchAgent (under a test label) and checks that launchd
    /// accepts it: `cargo test launch_agent_is_valid -- --ignored`.
    #[test]
    #[ignore]
    fn launch_agent_is_valid() {
        let dir = launch_agents_dir().unwrap();
        let exe = env::current_exe().unwrap();
        assert!(write_agent(&dir, TEST_LABEL, &exe));
        let lint = Command::new("/usr/bin/plutil").arg("-lint").arg(agent_file(&dir, TEST_LABEL)).status().unwrap();
        assert!(remove_agent(&dir, TEST_LABEL));
        assert!(!agent_file(&dir, TEST_LABEL).exists());
        assert!(lint.success());
    }

    /// Wraps this test binary in a temporary app bundle and toggles its login item from there:
    /// `cargo test login_item_round_trip -- --ignored --nocapture`. Registers a real login
    /// item for a moment; macOS may show a notification about it.
    #[test]
    #[ignore]
    fn login_item_round_trip() {
        let app = env::temp_dir().join(format!("WootlightTest-{}.app", std::process::id()));
        let _ = fs::remove_dir_all(&app);
        let macos = app.join("Contents/MacOS");
        fs::create_dir_all(&macos).unwrap();
        let exe = macos.join("WootlightTest");
        fs::copy(env::current_exe().unwrap(), &exe).unwrap();
        fs::write(
            app.join("Contents/Info.plist"),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>{TEST_LABEL}</string>
<key>CFBundleExecutable</key><string>WootlightTest</string>
<key>CFBundleName</key><string>WootlightTest</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>LSUIElement</key><true/>
</dict></plist>
"#
            ),
        )
        .unwrap();
        let signed = Command::new("/usr/bin/codesign").args(["--force", "--sign", "-"]).arg(&app).status().unwrap();
        assert!(signed.success());
        let output = Command::new(&exe)
            .args(["autostart::tests::login_item_child", "--exact", "--ignored", "--nocapture"])
            .env(CHILD, "1")
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&output.stdout);
        println!("{text}{}", String::from_utf8_lossy(&output.stderr));
        let _ = fs::remove_dir_all(&app);
        assert!(text.contains("login item: off -> on -> off"), "child did not finish");
    }

    #[test]
    #[ignore]
    fn login_item_child() {
        if env::var(CHILD).is_err() {
            return;
        }
        let bundle = bundle_path().expect("runs from the test bundle");
        println!("bundle {}", bundle.display());
        // Never touch the user's real LaunchAgent from here.
        assert!(!is_enabled(), "test login item already registered");
        assert!(set_login_item(true));
        let on = is_enabled();
        println!("status after register: {:?}", status());
        assert!(set_login_item(false));
        let off = !is_enabled();
        println!("status after unregister: {:?}", status());
        assert!(on && off);
        println!("login item: off -> on -> off");
    }
}
