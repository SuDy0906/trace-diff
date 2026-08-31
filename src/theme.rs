//! Color themes and NO_COLOR / forced-color handling.

use ratatui::style::Color;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum ThemeName {
    #[default]
    Default,
    Ocean,
    Amber,
    Mono,
}

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub name: ThemeName,
    pub brand: Color,
    pub ok: Color,
    pub warn: Color,
    pub critical: Color,
    pub muted: Color,
    pub accent: Color,
    pub use_color: bool,
}

impl Theme {
    pub fn resolve(preferred: ThemeName, no_color: bool, force_color: bool) -> Self {
        let env_no = std::env::var_os("NO_COLOR").is_some();
        let use_color = force_color || !(no_color || env_no);
        let mut t = Self::named(preferred);
        if !use_color {
            t = Self::mono();
            t.use_color = false;
        }
        t
    }

    pub fn named(name: ThemeName) -> Self {
        match name {
            ThemeName::Default => Self {
                name,
                brand: Color::Cyan,
                ok: Color::Green,
                warn: Color::Yellow,
                critical: Color::Red,
                muted: Color::DarkGray,
                accent: Color::White,
                use_color: true,
            },
            ThemeName::Ocean => Self {
                name,
                brand: Color::Rgb(56, 189, 248),
                ok: Color::Rgb(52, 211, 153),
                warn: Color::Rgb(251, 191, 36),
                critical: Color::Rgb(248, 113, 113),
                muted: Color::Rgb(100, 116, 139),
                accent: Color::Rgb(226, 232, 240),
                use_color: true,
            },
            ThemeName::Amber => Self {
                name,
                brand: Color::Rgb(245, 158, 11),
                ok: Color::Rgb(134, 239, 172),
                warn: Color::Rgb(251, 146, 60),
                critical: Color::Rgb(239, 68, 68),
                muted: Color::Rgb(120, 113, 108),
                accent: Color::Rgb(250, 250, 249),
                use_color: true,
            },
            ThemeName::Mono => Self::mono(),
        }
    }

    fn mono() -> Self {
        Self {
            name: ThemeName::Mono,
            brand: Color::White,
            ok: Color::White,
            warn: Color::White,
            critical: Color::White,
            muted: Color::Gray,
            accent: Color::White,
            use_color: true,
        }
    }

    pub fn cycle(self) -> Self {
        let next = match self.name {
            ThemeName::Default => ThemeName::Ocean,
            ThemeName::Ocean => ThemeName::Amber,
            ThemeName::Amber => ThemeName::Mono,
            ThemeName::Mono => ThemeName::Default,
        };
        if self.use_color {
            Self::named(next)
        } else {
            self
        }
    }

    /// Journey / timing heat color (respects mono / NO_COLOR).
    pub fn stage_heat_color(&self, ms: f64) -> Color {
        if !self.use_color {
            return self.muted;
        }
        if ms < 50.0 {
            Color::Green
        } else if ms < 150.0 {
            Color::Cyan
        } else if ms < 400.0 {
            Color::Yellow
        } else {
            Color::Red
        }
    }
}
