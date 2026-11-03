# macOS (Apple Silicon) port: team contract

Branch `macos` of a fork of https://github.com/Swiftlyx/Wootlight. Target: aarch64-apple-darwin,
macOS 14.4+ (Core Audio process taps).

## How the port is wired

- Portable code is shared unchanged: `dsp`, `layout`, `render`, `settings`, `engine`, `ui/{design,
  palette, spring, i18n, geometry}`.
- Windows-only modules are swapped on macOS with `#[cfg_attr(target_os = "macos", path = ...)]`:
  `src/{audio,autostart,media,player,tray}.rs` -> `src/mac/*.rs`, `src/win.rs` -> `src/mac/platform.rs`,
  `src/ui/{canvas,frame_clock,icon,tray_menu,window}.rs` -> `src/ui/mac/*.rs`.
- Small differences live inline behind `#[cfg(windows)]` / `#[cfg(target_os = "macos")]`
  (`main.rs`, `engine.rs`, `sdk.rs`, `paths.rs`, `logging.rs`, `ui/mod.rs`).
- Each mac module must keep the public API its Windows twin offers to shared code.

## Rules for every change

1. `cargo build` and `cargo test` pass on macOS.
2. `cargo check --target x86_64-pc-windows-msvc` stays clean: the Windows build must not regress.
   Moving portable logic out of a Windows file into a shared one is welcome; behaviour on
   Windows must not change.
3. Match upstream style: short doc comments, `// SAFETY:` on every unsafe block, `log!` for events.
4. Add macOS dependencies only under `[target.'cfg(target_os = "macos")'.dependencies]`.
5. Settings, log and extracted SDK live in `~/Library/Application Support/Wootlight`.
