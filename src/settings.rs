//! User settings stored as TOML in Wootlight.toml next to the executable.

use std::{fs, io, path::Path};

use serde::Deserialize;

use crate::effects::Effect;

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    /// "en" or "uk".
    pub language: String,
    /// "auto", "light" or "dark".
    pub theme: String,
    pub color: String,
    /// The user's own color, restored when "Custom" is picked again.
    pub custom_color: String,
    /// Custom colors the user saved, shown after "Custom".
    pub saved_colors: Vec<String>,
    pub brightness: f32,
    pub range_db: f32,
    pub headroom_db: f32,
    pub floor_db: f32,
    pub tilt_db: f32,
    pub equalize: f32,
    pub fmin: f32,
    pub fmax: f32,
    /// Analysis rate while the window is closed; the keyboard takes frames as fast as it can.
    pub fps: f32,
    pub hue_speed: f32,
    pub audio_device: String,
    /// Listen only to the player that reports playback to the Windows media controls.
    pub media_only: bool,
    /// Hand the lighting back to the keyboard while nothing plays.
    pub release_when_silent: bool,
    pub release_after_sec: f32,
    /// Animation while no music plays: "off" or an `Effect` code.
    pub effect: String,
    /// "idle": after `effect_after_sec` without keyboard or mouse input; "always": whenever nothing plays.
    pub effect_when: String,
    pub effect_after_sec: f32,
    /// "natural", "scheme" (the visualizer's color) or "custom" (`effect_custom_color`).
    pub effect_color: String,
    /// The user's own animation color: one hex color or "#base>#highlight".
    pub effect_custom_color: String,
    /// The arrow keys steer the snake effect without stopping it.
    pub snake_play: bool,
    /// "auto" installs new releases from GitHub, "notify" only shows them, "off" never asks.
    pub updates: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            language: "en".into(),
            theme: "auto".into(),
            color: "classic".into(),
            custom_color: "#c8c8c8>#ffb000".into(),
            saved_colors: Vec::new(),
            brightness: 1.0,
            range_db: 30.0,
            headroom_db: 3.0,
            floor_db: -75.0,
            tilt_db: 2.0,
            equalize: 1.0,
            fmin: 20.0,
            fmax: 20000.0,
            fps: 120.0,
            hue_speed: 0.08,
            audio_device: String::new(),
            media_only: false,
            release_when_silent: true,
            release_after_sec: 3.0,
            effect: "off".into(),
            effect_when: "idle".into(),
            effect_after_sec: 60.0,
            effect_color: "natural".into(),
            effect_custom_color: "#1060ff>#ffffff".into(),
            snake_play: false,
            updates: "auto".into(),
        }
    }
}

/// The most saved custom colors the settings keep.
pub const MAX_SAVED_COLORS: usize = 11;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ColorMode {
    Classic,
    Rainbow,
    Flow,
    Solid([f32; 3]),
    /// Bottom-row color to top-row color.
    Gradient([f32; 3], [f32; 3]),
}

/// "classic", "rainbow", "flow", "#rrggbb" or "#bottom>#top"; anything else is classic.
pub fn parse_color_mode(text: &str) -> ColorMode {
    let text = text.trim().to_ascii_lowercase();
    match text.as_str() {
        "rainbow" => ColorMode::Rainbow,
        "flow" => ColorMode::Flow,
        other => match other.split_once('>') {
            Some((bottom, top)) => match (parse_hex(bottom), parse_hex(top)) {
                (Some(b), Some(t)) => ColorMode::Gradient(b, t),
                _ => ColorMode::Classic,
            },
            None => parse_hex(other).map_or(ColorMode::Classic, ColorMode::Solid),
        },
    }
}

pub fn parse_hex(text: &str) -> Option<[f32; 3]> {
    let hex = text.trim().trim_start_matches('#');
    if hex.len() != 6 || !hex.is_ascii() {
        return None;
    }
    let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok().map(f32::from);
    Some([channel(0)?, channel(2)?, channel(4)?])
}

/// macOS wording for the comments that name Windows features; looked up before `COMMENTS`.
#[cfg(target_os = "macos")]
const PLATFORM_COMMENTS: &[(&str, &str, &str)] = &[
    (
        "audio_device",
        "Output device to show (part of its name). Empty follows the macOS default.",
        "Пристрій виводу, звук якого показувати (частина назви). Порожньо — пристрій macOS за замовчуванням.",
    ),
    (
        "media_only",
        "Listen only to the player that shows playback in Now Playing (Apple Music, Spotify, YouTube...).\nVoice chat, games and system sounds are left out.",
        "Слухати лише плеєр, що показує відтворення в «Зараз відтворюється» (Apple Music, Spotify, YouTube...).\nГолосовий чат, ігри та системні звуки не враховуються.",
    ),
];
#[cfg(not(target_os = "macos"))]
const PLATFORM_COMMENTS: &[(&str, &str, &str)] = &[];

/// Comments for the settings file: key, English, Ukrainian. Lines are separated by '\n'.
const COMMENTS: &[(&str, &str, &str)] = &[
    (
        "header",
        "Wootlight settings. Changes apply as soon as the file is saved.",
        "Налаштування Wootlight. Зміни застосовуються одразу після збереження файлу.",
    ),
    ("enabled", "Visualizer on (false restores the keyboard's own lighting).", "Увімкнути візуалізатор (false — повернути звичайну підсвітку)."),
    ("language", "Interface language: \"en\" or \"uk\".", "Мова інтерфейсу: \"en\" або \"uk\"."),
    ("theme", "Window theme: \"auto\", \"light\" or \"dark\".", "Тема вікна: \"auto\", \"light\" або \"dark\"."),
    (
        "color",
        "Color: \"classic\", \"rainbow\", \"flow\", one hex color like \"#00ffaa\",\nor bottom to top like \"#c8c8c8>#ff2a8a\" (white to pink).",
        "Колір: \"classic\", \"rainbow\", \"flow\", один hex-колір, наприклад \"#00ffaa\",\nабо перехід знизу вгору, наприклад \"#c8c8c8>#ff2a8a\" (білий у рожевий).",
    ),
    (
        "custom_color",
        "Your own color, used when \"Custom\" is picked: one hex color or \"#bottom>#top\".",
        "Ваш колір для пункту «Свій»: один hex-колір або \"#низ>#верх\".",
    ),
    (
        "saved_colors",
        "Saved custom colors, shown after \"Custom\": a list of hex colors or \"#bottom>#top\".",
        "Збережені свої кольори, що йдуть після «Свій»: список hex-кольорів або \"#низ>#верх\".",
    ),
    ("brightness", "Brightness from 0.0 to 1.0.", "Яскравість від 0.0 до 1.0."),
    (
        "range_db",
        "dB range from the bottom to the top row: lower is more contrast, higher is smoother.",
        "Діапазон у дБ від низу до верху: менше — контрастніше, більше — рівніше.",
    ),
    (
        "headroom_db",
        "How far in dB the loudest band (averaged over about a second) sits below the F-key row.\n0 keeps it near the top, 8 lets only strong hits reach it.",
        "На скільки дБ найгучніша смуга (в середньому за ~секунду) стоїть нижче ряду F.\n0 — майже постійно біля верху, 8 — верх лише на сильних ударах.",
    ),
    ("floor_db", "Level in dBFS treated as silence.", "Рівень у dBFS, який вважається тишею."),
    ("tilt_db", "Treble boost in dB per octave around 1 kHz.", "Підсилення високих частот, дБ на октаву відносно 1 кГц."),
    (
        "equalize",
        "Evening out from 0.0 to 1.0: each band's recent average is pulled to a common level,\nso the whole width moves. 0 shows the true spectrum.",
        "Вирівнювання від 0.0 до 1.0: середній рівень кожної смуги підтягується до спільного,\nщоб рухалася вся ширина. 0 — чесний спектр.",
    ),
    ("fmin", "Frequency range in Hz.", "Діапазон частот, Гц."),
    (
        "fps",
        "Analysis updates per second while the window is closed (it follows the display while open).",
        "Частота аналізу, поки вікно закрите (з відкритим вікном вона дорівнює частоті монітора).",
    ),
    ("hue_speed", "Color turns per second for \"flow\".", "Швидкість зміни кольору для \"flow\", обертів за секунду."),
    (
        "audio_device",
        "Output device to show (part of its name). Empty follows the Windows default.",
        "Пристрій виводу, звук якого показувати (частина назви). Порожньо — пристрій Windows за замовчуванням.",
    ),
    (
        "media_only",
        "Listen only to the player that shows playback in the Windows media controls (YouTube Music, Spotify...).\nVoice chat, games and system sounds are left out.",
        "Слухати лише плеєр, що показує відтворення в медіаелементах Windows (YouTube Music, Spotify...).\nГолосовий чат, ігри та системні звуки не враховуються.",
    ),
    (
        "release_when_silent",
        "While nothing plays, show the keyboard's own lighting (its Wootility profile).",
        "Коли нічого не грає, показувати власну підсвітку клавіатури (профіль із Wootility).",
    ),
    (
        "release_after_sec",
        "Seconds of silence before the lighting goes back to the keyboard.",
        "Скільки секунд тиші чекати, перш ніж повернути підсвітку клавіатурі.",
    ),
    (
        "effect",
        "Animation while no music plays: \"off\", \"rain\", \"storm\", \"matrix\", \"snow\", \"stars\", \"aurora\",\n\"ripples\", \"fire\" or \"snake\".",
        "Анімація, поки не грає музика: \"off\", \"rain\", \"storm\", \"matrix\", \"snow\", \"stars\", \"aurora\",\n\"ripples\", \"fire\" або \"snake\".",
    ),
    (
        "effect_when",
        "\"idle\": after effect_after_sec without keyboard or mouse input, until you use them again or the\nkeyboard's Dim After passes. \"always\": whenever no music plays.",
        "\"idle\": через effect_after_sec без клавіатури й миші, доки ними не скористатися або не мине\nDim After клавіатури. \"always\": завжди, коли не грає музика.",
    ),
    (
        "effect_after_sec",
        "Seconds without keyboard or mouse input before the animation starts.",
        "Скільки секунд без клавіатури й миші чекати, перш ніж запустити анімацію.",
    ),
    (
        "effect_color",
        "Animation colors: \"natural\", \"scheme\" (the visualizer's color) or \"custom\".",
        "Кольори анімації: \"natural\" (природні), \"scheme\" (як у візуалізатора) або \"custom\" (свої).",
    ),
    (
        "snake_play",
        "While the snake effect runs, the arrow keys steer it and do not stop it; other keys and the mouse do.",
        "Поки йде ефект «Змійка», стрілки керують нею і не зупиняють її; інші клавіші й миша зупиняють.",
    ),
    (
        "updates",
        "Updates from GitHub: \"auto\" downloads new versions and installs them while you are away or at\nthe next start, \"notify\" only shows them, \"off\" never contacts GitHub.",
        "Оновлення з GitHub: \"auto\" завантажує нові версії й встановлює їх, коли вас немає, або під час\nнаступного запуску, \"notify\" лише показує їх, \"off\" ніколи не звертається до GitHub.",
    ),
    (
        "effect_custom_color",
        "Your own animation color for \"custom\": one hex color or \"#base>#highlight\".",
        "Ваш колір анімації для \"custom\": один hex-колір або \"#основний>#відблиск\".",
    ),
];

fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

impl Settings {
    pub fn color_mode(&self) -> ColorMode {
        parse_color_mode(&self.color)
    }

    pub fn effect(&self) -> Option<Effect> {
        Effect::parse(&self.effect)
    }

    pub fn effect_always(&self) -> bool {
        self.effect_when == "always"
    }

    pub fn ukrainian(&self) -> bool {
        self.language.trim().eq_ignore_ascii_case("uk")
    }

    /// Clamps values to ranges the engine can handle.
    pub fn sanitized(mut self) -> Self {
        let finite_or = |v: f32, d: f32| if v.is_finite() { v } else { d };
        let d = Settings::default();
        self.language = if self.ukrainian() { "uk".into() } else { "en".into() };
        self.theme = match self.theme.trim().to_ascii_lowercase().as_str() {
            t @ ("light" | "dark") => t.into(),
            _ => "auto".into(),
        };
        self.brightness = finite_or(self.brightness, d.brightness).clamp(0.0, 1.0);
        self.range_db = finite_or(self.range_db, d.range_db).clamp(6.0, 90.0);
        self.headroom_db = finite_or(self.headroom_db, d.headroom_db).clamp(0.0, self.range_db / 2.0);
        self.floor_db = finite_or(self.floor_db, d.floor_db).clamp(-120.0, -20.0);
        self.tilt_db = finite_or(self.tilt_db, d.tilt_db).clamp(-6.0, 6.0);
        self.equalize = finite_or(self.equalize, d.equalize).clamp(0.0, 1.0);
        self.fmin = finite_or(self.fmin, d.fmin).clamp(20.0, 2000.0);
        self.fmax = finite_or(self.fmax, d.fmax).clamp(self.fmin * 4.0, 22000.0);
        self.fps = finite_or(self.fps, d.fps).clamp(30.0, 360.0);
        let own = |code: &str| matches!(parse_color_mode(code), ColorMode::Solid(_) | ColorMode::Gradient(..));
        if !own(&self.custom_color) {
            self.custom_color = d.custom_color;
        }
        self.saved_colors.retain(|code| own(code));
        self.saved_colors.truncate(MAX_SAVED_COLORS);
        self.hue_speed = finite_or(self.hue_speed, d.hue_speed).clamp(-5.0, 5.0);
        self.audio_device = self.audio_device.trim().to_string();
        self.release_after_sec = finite_or(self.release_after_sec, d.release_after_sec).clamp(0.5, 600.0);
        self.effect = self.effect().map_or("off", Effect::code).into();
        self.effect_when = if self.effect_when.trim().eq_ignore_ascii_case("always") { "always" } else { "idle" }.into();
        self.effect_after_sec = finite_or(self.effect_after_sec, d.effect_after_sec).clamp(10.0, 3600.0);
        self.effect_color = match self.effect_color.trim().to_ascii_lowercase().as_str() {
            c @ ("scheme" | "custom") => c.into(),
            _ => "natural".into(),
        };
        if !own(&self.effect_custom_color) {
            self.effect_custom_color = d.effect_custom_color;
        }
        self.updates = match self.updates.trim().to_ascii_lowercase().as_str() {
            u @ ("notify" | "off") => u.into(),
            _ => "auto".into(),
        };
        self
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
        toml::from_str::<Settings>(&text).map(Settings::sanitized).map_err(|e| match e.span() {
            Some(span) => {
                let line = text[..span.start.min(text.len())].matches('\n').count() + 1;
                format!("line {line}: {}", e.message())
            }
            None => e.message().to_string(),
        })
    }

    /// Loads a backup, refusing a TOML file that is not Wootlight's settings.
    pub fn load_backup(path: &Path) -> Result<Self, String> {
        let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
        let table: toml::Table = toml::from_str(&text).map_err(|e| e.message().to_string())?;
        if !["enabled", "color", "brightness"].iter().all(|key| table.contains_key(*key)) {
            return Err("this is not a Wootlight settings file".into());
        }
        Self::load(path)
    }

    /// Rewrites the file when it lacks keys or comments of the current version.
    pub fn upgrade_file(&self, path: &Path) -> io::Result<()> {
        if fs::read_to_string(path).is_ok_and(|text| text == self.to_toml()) {
            return Ok(());
        }
        self.save(path)
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(path, self.to_toml())
    }

    fn to_toml(&self) -> String {
        let uk = self.ukrainian();
        let comment = |key: &str| {
            PLATFORM_COMMENTS
                .iter()
                .chain(COMMENTS)
                .find(|(k, _, _)| *k == key)
                .map(|(_, en, ukr)| if uk { *ukr } else { *en })
                .unwrap_or_default()
                .lines()
                .map(|line| format!("# {line}\n"))
                .collect::<String>()
        };
        let values = [
            ("enabled", self.enabled.to_string()),
            ("language", quote(&self.language)),
            ("theme", quote(&self.theme)),
            ("color", quote(&self.color)),
            ("custom_color", quote(&self.custom_color)),
            ("saved_colors", format!("[{}]", self.saved_colors.iter().map(|c| quote(c)).collect::<Vec<_>>().join(", "))),
            ("brightness", format!("{:?}", self.brightness)),
            ("range_db", format!("{:?}", self.range_db)),
            ("headroom_db", format!("{:?}", self.headroom_db)),
            ("floor_db", format!("{:?}", self.floor_db)),
            ("tilt_db", format!("{:?}", self.tilt_db)),
            ("equalize", format!("{:?}", self.equalize)),
            ("fmin", format!("{:?}", self.fmin)),
            ("fmax", format!("{:?}", self.fmax)),
            ("fps", format!("{:?}", self.fps)),
            ("hue_speed", format!("{:?}", self.hue_speed)),
            ("audio_device", quote(&self.audio_device)),
            ("media_only", self.media_only.to_string()),
            ("release_when_silent", self.release_when_silent.to_string()),
            ("release_after_sec", format!("{:?}", self.release_after_sec)),
            ("effect", quote(&self.effect)),
            ("effect_when", quote(&self.effect_when)),
            ("effect_after_sec", format!("{:?}", self.effect_after_sec)),
            ("effect_color", quote(&self.effect_color)),
            ("effect_custom_color", quote(&self.effect_custom_color)),
            ("snake_play", self.snake_play.to_string()),
            ("updates", quote(&self.updates)),
        ];
        let mut out = comment("header");
        for (key, value) in &values {
            // fmax shares the comment above fmin.
            if *key != "fmax" {
                out.push('\n');
                out.push_str(&comment(key));
            }
            out.push_str(&format!("{key} = {value}\n"));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_round_trip_in_both_languages() {
        for language in ["en", "uk"] {
            let s = Settings {
                language: language.into(),
                color: "#c8c8c8>#ff2a8a".into(),
                saved_colors: vec!["#00ffaa".into(), "#c8c8c8>#9020ff".into()],
                audio_device: r#"Hi-Fi "Cable" \ (Game)"#.into(),
                brightness: 0.75,
                ..Settings::default()
            };
            let parsed: Settings = toml::from_str(&s.to_toml()).unwrap();
            assert_eq!(parsed, s);
            assert_eq!(parsed.color_mode(), ColorMode::Gradient([200.0, 200.0, 200.0], [255.0, 42.0, 138.0]));
        }
    }

    #[test]
    fn backups_must_be_wootlight_settings() {
        let dir = std::env::temp_dir().join(format!("wootlight-backup-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let backup = dir.join("backup.toml");
        let s = Settings { color: "#00ffaa".into(), effect: "snow".into(), ..Settings::default() };
        s.save(&backup).unwrap();
        assert_eq!(Settings::load_backup(&backup), Ok(s));
        let other = dir.join("Cargo.toml");
        fs::write(&other, "[package]\nname = \"x\"\n").unwrap();
        assert!(Settings::load_backup(&other).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_keys_use_defaults() {
        let parsed: Settings = toml::from_str("brightness = 0.5
mirror = true").unwrap();
        assert_eq!(parsed.brightness, 0.5);
        assert_eq!(parsed.range_db, Settings::default().range_db);
        assert_eq!(parsed.language, "en");
    }

    #[test]
    fn sanitize_normalizes_language_and_theme() {
        let s = Settings { language: "UK".into(), theme: "Neon".into(), ..Settings::default() }.sanitized();
        assert_eq!((s.language.as_str(), s.theme.as_str()), ("uk", "auto"));
    }

    #[test]
    fn sanitize_normalizes_effects() {
        let s = Settings {
            effect: " Matrix ".into(),
            effect_when: "ALWAYS".into(),
            effect_after_sec: 1.0,
            effect_color: "neon".into(),
            effect_custom_color: "rainbow".into(),
            ..Settings::default()
        }
        .sanitized();
        assert_eq!((s.effect.as_str(), s.effect_when.as_str(), s.effect_color.as_str()), ("matrix", "always", "natural"));
        assert_eq!(s.effect_after_sec, 10.0);
        assert_eq!(s.effect_custom_color, Settings::default().effect_custom_color);
        assert_eq!(Settings { effect: "lava".into(), ..Settings::default() }.sanitized().effect(), None);
    }

    #[test]
    fn sanitize_keeps_only_valid_saved_colors() {
        let mut saved: Vec<String> = vec!["rainbow".into(), "#12345".into(), "#c8c8c8>#ff1010".into()];
        saved.extend((0..20).map(|i| format!("#0000{i:02x}")));
        let s = Settings { saved_colors: saved, ..Settings::default() }.sanitized();
        assert_eq!(s.saved_colors.len(), MAX_SAVED_COLORS);
        assert_eq!(s.saved_colors[0], "#c8c8c8>#ff1010");
        assert_eq!(s.saved_colors[1], "#000000");
    }
}
