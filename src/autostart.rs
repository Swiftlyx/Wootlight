//! Start with Windows through a Task Scheduler task that runs at sign-in.
//!
//! Earlier versions used HKCU\...\Run, which Explorer may skip at sign-in, and a task under the
//! old app name; both are moved to the current task on start.

use std::{
    env, fs,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    ptr,
    sync::atomic::{AtomicU8, Ordering},
};

use windows_sys::Win32::{
    Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS},
    System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW},
};

use crate::{log, win::wide};

const TASK_NAME: &str = "Wootlight";
/// Task name of versions before the rename.
const LEGACY_TASK_NAME: &str = "WootingEQ";
const LEGACY_RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const LEGACY_VALUE_NAME: &str = "WootingEQ";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const UNKNOWN: u8 = 0;
const OFF: u8 = 1;
const ON: u8 = 2;
/// Whether the task exists, as last seen or set by this process.
static STATE: AtomicU8 = AtomicU8::new(UNKNOWN);

fn read(key: &str, name: &str) -> Option<String> {
    let key = wide(key);
    let name = wide(name);
    let mut size = 0u32;
    // SAFETY: size query first, then a buffer of that size.
    unsafe {
        if RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut size,
        ) != ERROR_SUCCESS
        {
            return None;
        }
        let mut buf = vec![0u16; (size as usize).div_ceil(2)];
        if RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut size,
        ) != ERROR_SUCCESS
        {
            return None;
        }
        let len = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..len]))
    }
}

fn delete(key: &str, name: &str) -> bool {
    let key = wide(key);
    let name = wide(name);
    // SAFETY: valid null-terminated strings.
    let result = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr()) };
    result == ERROR_SUCCESS || result == ERROR_FILE_NOT_FOUND
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

/// Task definition: runs `exe` when `user` signs in, without elevation and without the
/// default 72-hour run limit.
fn task_xml(exe: &Path, user: &str) -> String {
    let command = xml_escape(&format!("\"{}\"", exe.display()));
    let dir = xml_escape(&exe.parent().map(|d| d.display().to_string()).unwrap_or_default());
    let user = xml_escape(user);
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Starts Wootlight when you sign in.</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user}</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>4</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{command}</Command>
      <WorkingDirectory>{dir}</WorkingDirectory>
    </Exec>
  </Actions>
</Task>
"#
    )
}

/// Runs schtasks.exe without a console window; returns its exit status and output.
fn schtasks(args: &[&str]) -> Option<(bool, String)> {
    let exe = env::var_os("SystemRoot").map_or_else(|| PathBuf::from("schtasks.exe"), |root| PathBuf::from(root).join(r"System32\schtasks.exe"));
    let output = Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| log!("schtasks failed to start: {e}"))
        .ok()?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Some((output.status.success(), text))
}

/// The task's definition, if it exists.
fn query(name: &str) -> Option<String> {
    schtasks(&["/Query", "/TN", name, "/XML"]).and_then(|(ok, text)| ok.then_some(text))
}

fn create(name: &str, exe: &Path) -> bool {
    let (Some(domain), Some(user)) = (env::var_os("USERDOMAIN"), env::var_os("USERNAME")) else {
        log!("autostart: unknown user");
        return false;
    };
    let user = format!("{}\\{}", domain.to_string_lossy(), user.to_string_lossy());
    let file = env::temp_dir().join(format!("Wootlight-task-{}.xml", std::process::id()));
    // schtasks reads UTF-16 task files; the byte order mark tells it the encoding.
    let bytes: Vec<u8> = std::iter::once(0xFEFFu16).chain(task_xml(exe, &user).encode_utf16()).flat_map(u16::to_le_bytes).collect();
    if let Err(e) = fs::write(&file, bytes) {
        log!("autostart: cannot write the task file: {e}");
        return false;
    }
    let result = schtasks(&["/Create", "/TN", name, "/XML", &file.to_string_lossy(), "/F"]);
    let _ = fs::remove_file(&file);
    match result {
        Some((true, _)) => true,
        Some((false, text)) => {
            log!("autostart: creating the task failed: {}", text.trim());
            false
        }
        None => false,
    }
}

fn remove(name: &str) -> bool {
    query(name).is_none() || schtasks(&["/Delete", "/TN", name, "/F"]).is_some_and(|(ok, _)| ok)
}

pub fn is_enabled() -> bool {
    match STATE.load(Ordering::Relaxed) {
        UNKNOWN => {
            let on = query(TASK_NAME).is_some();
            STATE.store(if on { ON } else { OFF }, Ordering::Relaxed);
            on
        }
        state => state == ON,
    }
}

pub fn set_enabled(enabled: bool) -> bool {
    let ok = match (enabled, env::current_exe()) {
        (true, Ok(exe)) => create(TASK_NAME, &exe),
        (true, Err(_)) => false,
        (false, _) => remove(TASK_NAME),
    };
    if ok {
        delete(LEGACY_RUN_KEY, LEGACY_VALUE_NAME);
    }
    STATE.store(if !ok { UNKNOWN } else if enabled { ON } else { OFF }, Ordering::Relaxed);
    ok
}

/// Moves autostart entries of earlier versions to the task and points the task at the current
/// executable, e.g. after it was moved. Spawns schtasks: call it off the UI thread.
pub fn refresh() {
    let Ok(exe) = env::current_exe() else { return };
    let legacy_run = read(LEGACY_RUN_KEY, LEGACY_VALUE_NAME).is_some();
    let legacy_task = query(LEGACY_TASK_NAME).is_some();
    let current = query(TASK_NAME);
    let stale = current.as_ref().is_some_and(|xml| !xml.to_lowercase().contains(&exe.display().to_string().to_lowercase()));
    if legacy_run || legacy_task || stale {
        if create(TASK_NAME, &exe) {
            delete(LEGACY_RUN_KEY, LEGACY_VALUE_NAME);
            if legacy_task {
                remove(LEGACY_TASK_NAME);
            }
            let from = if legacy_task { "created from the old task" } else if legacy_run { "created from the Run entry" } else { "updated" };
            log!("autostart: task {from}");
            STATE.store(ON, Ordering::Relaxed);
            return;
        }
    }
    STATE.store(if current.is_some() { ON } else { OFF }, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_xml_quotes_and_escapes() {
        let xml = task_xml(Path::new(r"D:\Tools & Co\Wootlight.exe"), "PC\\me");
        assert!(xml.contains(r"<Command>&quot;D:\Tools &amp; Co\Wootlight.exe&quot;</Command>"));
        assert!(xml.contains(r"<WorkingDirectory>D:\Tools &amp; Co</WorkingDirectory>"));
        assert!(xml.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));
        assert_eq!(xml.matches(r"<UserId>PC\me</UserId>").count(), 2);
    }

    #[test]
    fn task_round_trip() {
        let name = "Wootlight.Test";
        let exe = env::current_exe().unwrap();
        assert!(create(name, &exe));
        let xml = query(name).expect("task exists");
        assert!(xml.to_lowercase().contains(&exe.display().to_string().to_lowercase()));
        assert!(remove(name));
        assert!(query(name).is_none());
        assert!(remove(name));
    }
}
