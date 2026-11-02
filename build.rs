//! Embeds the app icon and the version information in the executable as Windows resources.

use std::{env, fs, path::PathBuf, process::Command};

/// rc.exe from the `RC` variable or the newest Windows SDK.
fn rc_exe() -> Option<PathBuf> {
    if let Some(rc) = env::var_os("RC") {
        return Some(PathBuf::from(rc));
    }
    let root = PathBuf::from(env::var_os("ProgramFiles(x86)")?).join(r"Windows Kits\10\bin");
    let mut sdks: Vec<PathBuf> =
        fs::read_dir(root).ok()?.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.join(r"x64\rc.exe").is_file()).collect();
    sdks.sort();
    sdks.pop().map(|sdk| sdk.join(r"x64\rc.exe"))
}

fn resource_script(icon: &str) -> String {
    let v = |name: &str| env::var(name).unwrap_or_else(|_| "0".into());
    let (major, minor, patch) = (v("CARGO_PKG_VERSION_MAJOR"), v("CARGO_PKG_VERSION_MINOR"), v("CARGO_PKG_VERSION_PATCH"));
    let version = env::var("CARGO_PKG_VERSION").unwrap_or_default();
    // FILEOS 0x40004 is VOS_NT_WINDOWS32, FILETYPE 1 is VFT_APP: the script avoids SDK headers.
    format!(
        r#"1 ICON "{icon}"

1 VERSIONINFO
FILEVERSION {major},{minor},{patch},0
PRODUCTVERSION {major},{minor},{patch},0
FILEFLAGSMASK 0x3F
FILEFLAGS 0
FILEOS 0x40004
FILETYPE 1
FILESUBTYPE 0
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "Swiftlyx"
      VALUE "FileDescription", "Wootlight"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "Wootlight"
      VALUE "LegalCopyright", "Copyright (c) 2026 Swiftlyx. MIT License."
      VALUE "OriginalFilename", "Wootlight.exe"
      VALUE "ProductName", "Wootlight"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
    )
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/Wootlight.ico");
    println!("cargo:rerun-if-env-changed=RC");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let icon = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR")).join(r"assets\Wootlight.ico");
    let script = out.join("wootlight.rc");
    let res = out.join("wootlight.res");
    fs::write(&script, resource_script(&icon.display().to_string().replace('\\', "\\\\"))).expect("write the resource script");
    let Some(rc) = rc_exe() else {
        println!("cargo:warning=rc.exe not found (Windows SDK): building without the icon and version information");
        return;
    };
    match Command::new(&rc).arg("/nologo").arg("/fo").arg(&res).arg(&script).status() {
        Ok(status) if status.success() => println!("cargo:rustc-link-arg-bins={}", res.display()),
        result => println!("cargo:warning={} failed ({result:?}): building without the icon and version information", rc.display()),
    }
}
