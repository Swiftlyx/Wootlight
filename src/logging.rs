//! Minimal log to stderr and %LOCALAPPDATA%\Wootlight\wootlight.log (rewritten each start).

use std::{
    fs::{self, File},
    io::Write,
    sync::{Mutex, OnceLock},
    time::Instant,
};

use crate::paths;

static LOG: Mutex<Option<File>> = Mutex::new(None);
static START: OnceLock<Instant> = OnceLock::new();

pub fn init() {
    START.get_or_init(Instant::now);
    let dir = paths::local_dir();
    let _ = fs::create_dir_all(&dir);
    if let Ok(file) = File::create(dir.join("wootlight.log")) {
        *LOG.lock().unwrap_or_else(|e| e.into_inner()) = Some(file);
    }
}

pub fn write(args: std::fmt::Arguments) {
    let secs = START.get_or_init(Instant::now).elapsed().as_secs_f32();
    let line = format!("[{secs:9.2}] {args}");
    eprintln!("{line}");
    if let Some(file) = LOG.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        let _ = writeln!(file, "{line}");
    }
}

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => { $crate::logging::write(format_args!($($arg)*)) };
}
