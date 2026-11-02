# Changelog

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
