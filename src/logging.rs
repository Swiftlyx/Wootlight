//! Log to stderr and %LOCALAPPDATA%\Wootlight\wootlight.log (macOS: ~/Library/Application
//! Support/Wootlight/wootlight.log), one line per event with the local
//! time and the thread. Every start appends to the file; past `LIMIT` it moves to
//! wootlight.old.log. Panics and crashes are logged with the place they happened.

use std::{
    fmt,
    fs::{self, File, OpenOptions},
    io::Write,
    panic,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
    thread,
};
#[cfg(windows)]
use std::{ptr, sync::TryLockError};

#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::{EXCEPTION_ACCESS_VIOLATION, HMODULE, MAX_PATH, SYSTEMTIME},
    System::{
        Diagnostics::Debug::{EXCEPTION_POINTERS, SetUnhandledExceptionFilter},
        LibraryLoader::{
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, GetModuleFileNameW,
            GetModuleHandleExW,
        },
        SystemInformation::GetLocalTime,
    },
};

use crate::paths;

const FILE_NAME: &str = "wootlight.log";
const OLD_FILE_NAME: &str = "wootlight.old.log";
/// Size at which the log moves to the old file; the two together stay near 2 MiB.
const LIMIT: u64 = 1 << 20;
/// Lets Windows Error Reporting handle the crash after it is logged.
#[cfg(windows)]
const EXCEPTION_CONTINUE_SEARCH: i32 = 0;

struct Log {
    file: File,
    size: u64,
    path: PathBuf,
}

static LOG: Mutex<Option<Log>> = Mutex::new(None);

/// The current log file.
pub fn path() -> PathBuf {
    paths::local_dir().join(FILE_NAME)
}

fn open(path: &Path) -> Option<Log> {
    let file = OpenOptions::new().create(true).append(true).open(path).ok()?;
    let size = file.metadata().map_or(0, |m| m.len());
    #[cfg(target_os = "macos")]
    crash::set_file(std::os::fd::AsRawFd::as_raw_fd(&file));
    Some(Log { file, size, path: path.to_path_buf() })
}

/// Moves a full log to the old file, replacing it, and starts an empty one.
fn rotate(path: &Path) -> Option<Log> {
    let _ = fs::rename(path, path.with_file_name(OLD_FILE_NAME));
    open(path)
}

/// Opens the log and installs the panic and crash handlers.
pub fn init() {
    let path = path();
    let _ = path.parent().map(fs::create_dir_all);
    let log = match open(&path) {
        Some(log) if log.size >= LIMIT => {
            drop(log);
            rotate(&path)
        }
        log => log,
    };
    let mut slot = LOG.lock().unwrap_or_else(|e| e.into_inner());
    *slot = log;
    // A blank line between runs.
    if slot.as_ref().is_some_and(|log| log.size > 0) {
        append(&mut slot, "\n");
    }
    drop(slot);
    panic::set_hook(Box::new(on_panic));
    // SAFETY: installs a process-wide filter that stays valid for the process lifetime.
    #[cfg(windows)]
    unsafe { SetUnhandledExceptionFilter(Some(on_crash)) };
    #[cfg(target_os = "macos")]
    crash::install();
}

pub fn write(args: fmt::Arguments) {
    let line = format!("{} [{}] {args}\n", timestamp(), thread_name());
    eprint!("{line}");
    append(&mut LOG.lock().unwrap_or_else(|e| e.into_inner()), &line);
}

fn append(slot: &mut MutexGuard<Option<Log>>, line: &str) {
    let Some(log) = slot.as_mut() else { return };
    if log.file.write_all(line.as_bytes()).is_ok() {
        log.size += line.len() as u64;
    }
    if log.size >= LIMIT {
        // The file is closed before the rename.
        let path = log.path.clone();
        **slot = None;
        **slot = rotate(&path);
    }
}

#[cfg(windows)]
fn timestamp() -> String {
    // SAFETY: fills a local struct.
    let t = unsafe {
        let mut t: SYSTEMTIME = std::mem::zeroed();
        GetLocalTime(&mut t);
        t
    };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    )
}

#[cfg(target_os = "macos")]
fn timestamp() -> String {
    // SAFETY: localtime_r fills a local struct from the current time.
    let (t, ms) = unsafe {
        let mut tv: libc::timeval = std::mem::zeroed();
        libc::gettimeofday(&mut tv, std::ptr::null_mut());
        let mut t: libc::tm = std::mem::zeroed();
        libc::localtime_r(&tv.tv_sec, &mut t);
        (t, tv.tv_usec / 1000)
    };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        t.tm_year + 1900, t.tm_mon + 1, t.tm_mday, t.tm_hour, t.tm_min, t.tm_sec, ms
    )
}

fn thread_name() -> String {
    thread::current().name().unwrap_or("other").to_string()
}

fn on_panic(info: &panic::PanicHookInfo) {
    let message = info.payload_as_str().unwrap_or("no message");
    match info.location() {
        Some(at) => write(format_args!("PANIC: {message} (at {}:{})", at.file(), at.line())),
        None => write(format_args!("PANIC: {message}")),
    }
}

/// Logs a crash the Rust runtime cannot catch, such as an access violation in the SDK DLL.
#[cfg(windows)]
unsafe extern "system" fn on_crash(info: *const EXCEPTION_POINTERS) -> i32 {
    // SAFETY: Windows passes valid exception pointers to the filter.
    let record = unsafe { &*(*info).ExceptionRecord };
    let mut text = format!("CRASH: exception 0x{:08X} at {}", record.ExceptionCode as u32, place(record.ExceptionAddress as usize));
    if record.ExceptionCode == EXCEPTION_ACCESS_VIOLATION && record.NumberParameters >= 2 {
        let access = match record.ExceptionInformation[0] {
            0 => "reading",
            1 => "writing",
            _ => "executing",
        };
        text += &format!(", {access} address 0x{:X}", record.ExceptionInformation[1]);
    }
    let line = format!("{} [{}] {text}\n", timestamp(), thread_name());
    // The crashing thread may hold the log lock; skip the file rather than deadlock.
    let slot = match LOG.try_lock() {
        Ok(slot) => Some(slot),
        Err(TryLockError::Poisoned(e)) => Some(e.into_inner()),
        Err(TryLockError::WouldBlock) => None,
    };
    if let Some(mut slot) = slot {
        append(&mut slot, &line);
    }
    EXCEPTION_CONTINUE_SEARCH
}

/// `module+0xoffset` for a code address, or the bare address outside any module.
#[cfg(windows)]
fn place(address: usize) -> String {
    let mut module: HMODULE = ptr::null_mut();
    let flags = GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT;
    // SAFETY: with FROM_ADDRESS the name argument is an address; the reference count is unchanged.
    if unsafe { GetModuleHandleExW(flags, address as *const u16, &mut module) } == 0 {
        return format!("0x{address:X}");
    }
    let mut buf = [0u16; MAX_PATH as usize];
    // SAFETY: the buffer length is passed along.
    let len = unsafe { GetModuleFileNameW(module, buf.as_mut_ptr(), buf.len() as u32) } as usize;
    let path = String::from_utf16_lossy(&buf[..len.min(buf.len())]);
    let name = path.rsplit('\\').next().unwrap_or(&path);
    format!("{name}+0x{:X}", address - module as usize)
}

/// Logs fatal signals (SIGSEGV, SIGBUS, SIGILL, SIGFPE), such as a fault in the SDK library.
/// The handler only uses async-signal-safe calls: it formats into a stack buffer, `write`s it
/// to the log's descriptor and stderr, then hands the signal to the handler it replaced (Rust's
/// stack overflow handler) or to the default action, so the crash reporter still sees it.
/// The time is local time from an offset taken at start, without the lock the log uses.
#[cfg(target_os = "macos")]
mod crash {
    use std::{
        cell::UnsafeCell,
        ffi::{c_int, c_void},
        mem,
        sync::atomic::{AtomicI32, AtomicI64, Ordering},
    };

    const SIGNALS: [c_int; 4] = [libc::SIGSEGV, libc::SIGBUS, libc::SIGILL, libc::SIGFPE];

    /// Descriptor of the log file; -1 before the log opens.
    static FD: AtomicI32 = AtomicI32::new(-1);
    /// Seconds east of UTC when the handler was installed.
    static UTC_OFFSET: AtomicI64 = AtomicI64::new(0);

    /// Handlers replaced by `install`, written before any of ours can run.
    struct Previous(UnsafeCell<[mem::MaybeUninit<libc::sigaction>; 4]>);
    // SAFETY: written once in `install` before the handlers are installed, read-only afterwards.
    unsafe impl Sync for Previous {}
    static PREVIOUS: Previous = Previous(UnsafeCell::new([mem::MaybeUninit::uninit(); 4]));

    pub fn set_file(fd: c_int) {
        FD.store(fd, Ordering::SeqCst);
    }

    pub fn install() {
        // SAFETY: localtime_r fills a local struct; sigaction gets fully initialized structs, and
        // PREVIOUS is filled for a signal before our handler for it is installed.
        unsafe {
            let now = libc::time(std::ptr::null_mut());
            let mut tm: libc::tm = mem::zeroed();
            libc::localtime_r(&now, &mut tm);
            UTC_OFFSET.store(tm.tm_gmtoff, Ordering::SeqCst);
            let previous = &mut *PREVIOUS.0.get();
            for (i, &signal) in SIGNALS.iter().enumerate() {
                let mut action: libc::sigaction = mem::zeroed();
                action.sa_sigaction = on_signal as *const () as usize;
                action.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK;
                libc::sigemptyset(&mut action.sa_mask);
                if libc::sigaction(signal, std::ptr::null(), previous[i].as_mut_ptr()) == 0 {
                    libc::sigaction(signal, &action, std::ptr::null_mut());
                }
            }
        }
    }

    /// A fixed buffer formatted without allocating.
    struct Line {
        buf: [u8; 192],
        len: usize,
    }

    impl Line {
        fn text(&mut self, text: &[u8]) {
            for &b in text {
                if self.len < self.buf.len() {
                    self.buf[self.len] = b;
                    self.len += 1;
                }
            }
        }

        fn number(&mut self, mut n: u64, width: usize) {
            let mut digits = [0u8; 20];
            let mut i = digits.len();
            while n > 0 || digits.len() - i < width.max(1) {
                i -= 1;
                digits[i] = b'0' + (n % 10) as u8;
                n /= 10;
            }
            self.text(&digits[i..]);
        }

        fn hex(&mut self, mut n: usize) {
            let mut digits = [0u8; 16];
            let mut i = digits.len();
            loop {
                i -= 1;
                digits[i] = b"0123456789ABCDEF"[n & 15];
                n >>= 4;
                if n == 0 {
                    break;
                }
            }
            self.text(&digits[i..]);
        }

        /// "YYYY-MM-DD hh:mm:ss.mmm", as `timestamp` writes it.
        fn time(&mut self, seconds: i64, millis: u64) {
            let days = seconds.div_euclid(86_400);
            let secs = seconds.rem_euclid(86_400) as u64;
            // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
            let z = days + 719_468;
            let era = z.div_euclid(146_097);
            let doe = z.rem_euclid(146_097);
            let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
            let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
            let mp = (5 * doy + 2) / 153;
            let day = doy - (153 * mp + 2) / 5 + 1;
            let month = if mp < 10 { mp + 3 } else { mp - 9 };
            let year = yoe + era * 400 + i64::from(month <= 2);
            self.number(year.max(0) as u64, 4);
            self.text(b"-");
            self.number(month as u64, 2);
            self.text(b"-");
            self.number(day as u64, 2);
            self.text(b" ");
            self.number(secs / 3600, 2);
            self.text(b":");
            self.number(secs / 60 % 60, 2);
            self.text(b":");
            self.number(secs % 60, 2);
            self.text(b".");
            self.number(millis, 3);
        }
    }

    fn name(signal: c_int) -> &'static [u8] {
        match signal {
            libc::SIGSEGV => b"SIGSEGV",
            libc::SIGBUS => b"SIGBUS",
            libc::SIGILL => b"SIGILL",
            libc::SIGFPE => b"SIGFPE",
            _ => b"signal",
        }
    }

    extern "C" fn on_signal(signal: c_int, info: *mut libc::siginfo_t, context: *mut c_void) {
        let mut line = Line { buf: [0; 192], len: 0 };
        // SAFETY: only async-signal-safe calls (clock_gettime, write, sigaction); `info` comes
        // from the kernel; PREVIOUS was filled before this handler was installed.
        unsafe {
            let mut now: libc::timespec = mem::zeroed();
            libc::clock_gettime(libc::CLOCK_REALTIME, &mut now);
            line.time(now.tv_sec + UTC_OFFSET.load(Ordering::Relaxed), now.tv_nsec as u64 / 1_000_000);
            line.text(b" [crash] CRASH: ");
            line.text(name(signal));
            if !info.is_null() {
                line.text(b", address 0x");
                line.hex((*info).si_addr as usize);
            }
            line.text(b"\n");
            let fd = FD.load(Ordering::Relaxed);
            if fd >= 0 {
                libc::write(fd, line.buf.as_ptr().cast(), line.len);
            }
            libc::write(libc::STDERR_FILENO, line.buf.as_ptr().cast(), line.len);

            let Some(i) = SIGNALS.iter().position(|&s| s == signal) else { return };
            let previous = (*PREVIOUS.0.get())[i].assume_init_ref();
            let handler = previous.sa_sigaction;
            if handler == libc::SIG_DFL || handler == libc::SIG_IGN {
                // Back to the default action: the faulting instruction runs again and the
                // process ends with the usual crash report.
                let mut default: libc::sigaction = mem::zeroed();
                default.sa_sigaction = libc::SIG_DFL;
                libc::sigemptyset(&mut default.sa_mask);
                libc::sigaction(signal, &default, std::ptr::null_mut());
            } else if previous.sa_flags & libc::SA_SIGINFO != 0 {
                let previous: extern "C" fn(c_int, *mut libc::siginfo_t, *mut c_void) = mem::transmute(handler);
                previous(signal, info, context);
            } else {
                let previous: extern "C" fn(c_int) = mem::transmute(handler);
                previous(signal);
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn formats_dates() {
            let mut line = Line { buf: [0; 192], len: 0 };
            // 2026-11-02 23:05:09 UTC.
            line.time(1_793_660_709, 7);
            assert_eq!(&line.buf[..line.len], b"2026-11-02 23:05:09.007");
            let mut line = Line { buf: [0; 192], len: 0 };
            line.hex(0x10);
            line.number(0, 2);
            assert_eq!(&line.buf[..line.len], b"1000");
        }
    }
}

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => { $crate::logging::write(format_args!($($arg)*)) };
}

#[cfg(all(test, windows))]
mod tests {
    use std::{env, process::Command};

    use windows_sys::Win32::System::Diagnostics::Debug::{SEM_NOGPFAULTERRORBOX, SetErrorMode};

    use super::*;

    const CHILD: &str = "WOOTLIGHT_LOG_CHILD";

    /// Runs `crash_child` in a process of its own with a temporary %LOCALAPPDATA%, then reads its log:
    /// `cargo test crashes_are_logged -- --ignored`.
    #[test]
    #[ignore]
    fn crashes_are_logged() {
        for kind in ["panic", "fault"] {
            let dir = env::temp_dir().join(format!("wootlight-log-test-{kind}"));
            let _ = fs::remove_dir_all(&dir);
            let log_dir = dir.join("Wootlight");
            fs::create_dir_all(&log_dir).unwrap();
            // A full log from an earlier run moves aside on start.
            fs::write(log_dir.join(FILE_NAME), vec![b'x'; LIMIT as usize]).unwrap();
            let _ = Command::new(env::current_exe().unwrap())
                .args(["logging::tests::crash_child", "--exact", "--ignored", "--nocapture"])
                .env(CHILD, kind)
                .env("LOCALAPPDATA", &dir)
                .status()
                .unwrap();
            let log = fs::read_to_string(log_dir.join(FILE_NAME)).unwrap();
            println!("{log}");
            assert_eq!(fs::metadata(log_dir.join(OLD_FILE_NAME)).unwrap().len(), LIMIT);
            match kind {
                "panic" => assert!(log.contains("[worker] PANIC: test panic (at src") && log.contains("logging.rs:")),
                _ => assert!(log.contains("CRASH: exception 0xC0000005 at ") && log.contains("writing address 0x10")),
            }
        }
    }

    #[test]
    #[ignore]
    fn crash_child() {
        let Ok(kind) = env::var(CHILD) else { return };
        init();
        crate::log!("child started");
        if kind == "panic" {
            let _ = thread::Builder::new().name("worker".into()).spawn(|| panic!("test panic")).unwrap().join();
        } else {
            // SAFETY: suppresses the Windows Error Reporting dialog for the deliberate fault below.
            unsafe { SetErrorMode(SEM_NOGPFAULTERRORBOX) };
            // SAFETY: none; this write faults on purpose to exercise the crash filter.
            unsafe { ptr::write_volatile(0x10 as *mut u8, 1) };
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod mac_tests {
    use std::{env, process::Command};

    use super::*;

    const CHILD: &str = "WOOTLIGHT_LOG_CHILD";

    /// Runs `crash_child` in a process of its own with a temporary HOME, then reads its log:
    /// `cargo test crashes_are_logged -- --ignored`. The fault leaves a crash report behind in
    /// ~/Library/Logs/DiagnosticReports.
    #[test]
    #[ignore]
    fn crashes_are_logged() {
        for kind in ["panic", "fault"] {
            let home = env::temp_dir().join(format!("wootlight-log-test-{kind}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&home);
            let status = Command::new(env::current_exe().unwrap())
                .args(["logging::mac_tests::crash_child", "--exact", "--ignored", "--nocapture"])
                .env(CHILD, kind)
                .env("HOME", &home)
                .status()
                .unwrap();
            let log = fs::read_to_string(home.join("Library/Application Support/Wootlight").join(FILE_NAME)).unwrap();
            println!("{log}");
            let _ = fs::remove_dir_all(&home);
            match kind {
                "panic" => assert!(log.contains("[worker] PANIC: test panic (at src") && log.contains("logging.rs:")),
                _ => {
                    assert!(!status.success());
                    assert!(log.contains("[crash] CRASH: SIG") && log.contains(", address 0x10"), "{log}");
                }
            }
        }
    }

    #[test]
    #[ignore]
    fn crash_child() {
        let Ok(kind) = env::var(CHILD) else { return };
        init();
        crate::log!("child started");
        if kind == "panic" {
            let _ = thread::Builder::new().name("worker".into()).spawn(|| panic!("test panic")).unwrap().join();
        } else {
            // SAFETY: none; this write faults on purpose to exercise the signal handler.
            unsafe { std::ptr::write_volatile(0x10 as *mut u8, 1) };
        }
    }
}
