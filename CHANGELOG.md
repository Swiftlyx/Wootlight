# Changelog

## Unreleased

- A new *Effects* page with eleven animations for the time no music plays: rain, a thunderstorm,
  the Matrix, snow, a starry sky, an aurora, ripples, fire, the Game of Life, a clock and a snake
  that plays itself. *When idle*, an effect starts after a while without keyboard or mouse input
  and stops at the next input or when the keyboard's *Dim After* from Wootility passes; *Always*
  shows it whenever no music plays. Colors: the effect's own, the visualizer's, or your own solid
  color or gradient. While the page is open, the keyboard previews the effect.
- The snake plays a new game every time: it heads for the food without walling itself in, with a
  taste for detours of its own. *Play with the arrow keys* lets you steer it without stopping the
  effect.
- Updates from GitHub. *Automatic* downloads a new version, checks it against `SHA256SUMS.txt` and
  installs it while you are away or at the next start; *Notify* only shows it; *Off* never asks.
  On macOS the update opens the release page.
- *Settings → Back up settings* and *Restore settings* save all settings to a file and load them
  back.
- The log notes the keyboard's *Dim After*, why an effect started or stopped, and every update.

## 1.3.0 — 2026-11-03

- Wootlight runs on Macs with Apple Silicon and macOS 14.4 or later, in the menu bar, ported by
  Jason Bryner: Core Audio capture of the device or of the playing app alone, *Now Playing* for
  *React to: Media players*, and *Open at Login*. Releases get `Wootlight-<version>-macos-arm64.zip`,
  built on GitHub Actions with the Wooting RGB SDK rebuilt from its source.
- A frame the keyboard already shows is not sent again, so quiet passages send less over USB.

## 1.2.0 — 2026-11-03

- *Settings → Log* opens the log. Every line has the date, the time and the part of the app that
  wrote it. The log is kept across starts and moves to `wootlight.old.log` past 1 MB.
- The log says why the lighting went back to the keyboard: no player playing, the player silent,
  or no sound from the device. It also lists the players in the Windows media controls, the
  settings that decide when the visualizer runs, sleep and wake-up, and the Windows version.
- Crashes are logged with their place: a panic with its source line, a crash in the SDK or another
  DLL with the module and offset. A part of the app stuck for 5 seconds is logged with what it was
  doing.

## 1.1.0 — 2026-11-02

- *React to: Media players* now listens to the playing player alone, so voice chat in Discord,
  games and system sounds no longer move the bars while music plays. Wootlight finds the player's
  process from the Windows media controls and records that process only (Windows 10 version 2004
  or later); the status line names the player. When it cannot tell which process plays, it listens
  to the whole device as before.
- A purple icon.
- The README explains why the bars stay dim after Wootility's *Dim After* has dimmed the keyboard:
  only a key press lifts it.

## 1.0.0 — 2026-11-02

First public release.

- Equalizer bars on the lighting of Wooting One, Two, Two Lekker, Two HE, 60HE, UwU and 80HE, ANSI and
  ISO, fitted to the real key positions of each model.
- Bands spread evenly over the octaves from 20 Hz to 20 kHz, with sensitivity presets, four frequency
  ranges and *Even out frequencies*.
- Twelve color presets, a custom solid color or gradient, and up to 11 saved custom presets.
- *React to: Media players* lights the keyboard only while a player shows playback in the Windows
  media controls.
- The keyboard shows its own lighting during silence and when Wootlight quits.
- A window with a live keyboard or equalizer view that follows the monitor's refresh rate, in English
  and Ukrainian, with light and dark themes.
- *Start with Windows* through a Task Scheduler task, without administrator rights.
- Portable settings in `Wootlight.toml` next to the executable, applied as soon as the file is saved.
