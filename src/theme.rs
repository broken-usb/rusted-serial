//! Tema do app.
//!
//! Lê a cor de destaque e o modo claro/escuro do sistema UMA única vez,
//! na inicialização, e gera uma paleta levemente tingida com essa cor.
//! - Linux: portal `org.freedesktop.portal.Settings` (GNOME, KDE e outros), via D-Bus.
//! - Windows: registro do usuário (tema de apps + cor de destaque do DWM).

use eframe::egui::{self, Color32, CornerRadius, Stroke, Theme, Visuals};

/// Cor usada quando o sistema não informa uma cor de destaque.
pub const FALLBACK_ACCENT: Color32 = Color32::from_rgb(0xE8, 0x74, 0x3B);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
    Light,
    Dark,
}

/// O que conseguimos ler do sistema (cada campo pode faltar).
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemPrefs {
    pub scheme: Option<Scheme>,
    pub accent: Option<Color32>,
}

/// Consulta o sistema operacional. Não fica observando mudanças.
pub fn read_system() -> SystemPrefs {
    platform::read()
}

// ───────────────────────────── Linux ─────────────────────────────
#[cfg(target_os = "linux")]
mod platform {
    use super::{Scheme, SystemPrefs};
    use eframe::egui::Color32;
    use zbus::blocking::Connection;
    use zbus::zvariant::{OwnedValue, Value};

    const DEST: &str = "org.freedesktop.portal.Desktop";
    const PATH: &str = "/org/freedesktop/portal/desktop";
    const IFACE: &str = "org.freedesktop.portal.Settings";
    const NAMESPACE: &str = "org.freedesktop.appearance";

    pub fn read() -> SystemPrefs {
        let Ok(conn) = Connection::session() else {
            return SystemPrefs::default();
        };

        // color-scheme (u): 0 = sem preferência, 1 = escuro, 2 = claro
        let scheme = read_setting(&conn, "color-scheme")
            .and_then(|v| u32::try_from(v).ok())
            .and_then(|n| match n {
                1 => Some(Scheme::Dark),
                2 => Some(Scheme::Light),
                _ => None,
            });

        // accent-color ((ddd)): RGB sRGB em [0,1]; fora da faixa = não definido
        let accent = read_setting(&conn, "accent-color")
            .and_then(|v| <(f64, f64, f64)>::try_from(v).ok())
            .filter(|(r, g, b)| [r, g, b].iter().all(|c| (0.0..=1.0).contains(*c)))
            .map(|(r, g, b)| {
                let c = |x: f64| (x * 255.0).round() as u8;
                Color32::from_rgb(c(r), c(g), c(b))
            });

        SystemPrefs { scheme, accent }
        // `conn` é descartada aqui: nada continua rodando em segundo plano.
    }

    fn read_setting(conn: &Connection, key: &str) -> Option<OwnedValue> {
        // ReadOne: portais mais novos (>= 1.17).
        if let Ok(msg) =
            conn.call_method(Some(DEST), PATH, Some(IFACE), "ReadOne", &(NAMESPACE, key))
        {
            return msg.body().deserialize::<OwnedValue>().ok();
        }
        // Read (método antigo): o valor vem embrulhado em uma variante extra.
        let msg = conn
            .call_method(Some(DEST), PATH, Some(IFACE), "Read", &(NAMESPACE, key))
            .ok()?;
        let outer = msg.body().deserialize::<OwnedValue>().ok()?;
        match &*outer {
            Value::Value(inner) => inner.try_to_owned().ok(),
            _ => Some(outer),
        }
    }
}

// ──────────────────────────── Windows ────────────────────────────
#[cfg(windows)]
mod platform {
    use super::{Scheme, SystemPrefs};
    use eframe::egui::Color32;
    use windows_registry::CURRENT_USER;

    pub fn read() -> SystemPrefs {
        let scheme = CURRENT_USER
            .open(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
            .and_then(|k| k.get_u32("AppsUseLightTheme"))
            .ok()
            .map(|v| if v == 0 { Scheme::Dark } else { Scheme::Light });

        // AccentColor é um DWORD no formato 0xAABBGGRR.
        let accent = CURRENT_USER
            .open(r"Software\Microsoft\Windows\DWM")
            .and_then(|k| k.get_u32("AccentColor"))
            .ok()
            .map(|abgr| Color32::from_rgb(abgr as u8, (abgr >> 8) as u8, (abgr >> 16) as u8));

        SystemPrefs { scheme, accent }
    }
}

// ───────────────────────── Outros sistemas ─────────────────────────
#[cfg(not(any(target_os = "linux", windows)))]
mod platform {
    pub fn read() -> super::SystemPrefs {
        super::SystemPrefs::default()
    }
}

// ───────────────────────────── Paleta ─────────────────────────────

/// Todas as cores que a interface usa.
#[derive(Clone, Debug)]
pub struct Palette {
    pub scheme: Scheme,
    /// Cor de destaque pura (fundo de botões, logo, indicadores).
    pub accent: Color32,
    /// Texto sobre `accent` (preto ou branco, o que tiver mais contraste).
    pub on_accent: Color32,
    /// Destaque ajustado para ser legível como texto sobre o terminal.
    pub accent_text: Color32,
    /// `true` se a cor veio do sistema, `false` se é a cor padrão.
    pub accent_from_system: bool,

    pub bg: Color32,
    pub header: Color32,
    pub terminal: Color32,
    pub terminal_bar: Color32,
    pub field: Color32,
    pub border: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub faint: Color32,

    pub ok_bg: Color32,
    pub ok_dot: Color32,
    pub ok_text: Color32,
    pub idle_bg: Color32,
    pub error_text: Color32,
}

impl Palette {
    pub fn new(scheme: Scheme, accent: Option<Color32>) -> Self {
        let accent_from_system = accent.is_some();
        let a = accent.unwrap_or(FALLBACK_ACCENT);
        let t = |base: u32, amount: f32| mix(hex(base), a, amount);

        let (bg, header, terminal, terminal_bar, field, border, text, muted, faint) = match scheme {
            Scheme::Dark => (
                t(0x141518, 0.06),
                t(0x1B1C20, 0.08),
                t(0x0F1012, 0.05),
                t(0x17181C, 0.07),
                t(0x24262B, 0.10),
                t(0x30333A, 0.16),
                t(0xE8E9EC, 0.04),
                t(0xA0A4AC, 0.10),
                t(0x7A7F88, 0.10),
            ),
            Scheme::Light => (
                t(0xF5F5F7, 0.05),
                t(0xEAEAED, 0.08),
                t(0xFFFFFF, 0.025),
                t(0xF3F3F5, 0.06),
                t(0xFFFFFF, 0.04),
                t(0xCDCED3, 0.18),
                t(0x1C1E22, 0.05),
                t(0x50545C, 0.10),
                t(0x666A73, 0.10),
            ),
        };

        let (green, red) = match scheme {
            Scheme::Dark => (hex(0x4CC38A), hex(0xFF7B72)),
            Scheme::Light => (hex(0x1F8A4C), hex(0xC0262D)),
        };
        let ok_bg = mix(header, green, 0.16);

        Self {
            scheme,
            accent: a,
            on_accent: best_on(a),
            accent_text: ensure_contrast(a, terminal, 4.5),
            accent_from_system,
            bg,
            header,
            terminal,
            terminal_bar,
            field,
            border,
            text: ensure_contrast(text, terminal, 7.0),
            muted: ensure_contrast(muted, header, 4.5),
            faint: ensure_contrast(faint, terminal, 4.5),
            ok_bg,
            ok_dot: green,
            ok_text: ensure_contrast(green, ok_bg, 4.5),
            idle_bg: mix(header, faint, 0.18),
            error_text: ensure_contrast(red, terminal, 4.5),
        }
    }

    /// Aplica a paleta no egui (fixa o tema: não troca sozinho depois).
    pub fn apply(&self, ctx: &egui::Context) {
        let theme = match self.scheme {
            Scheme::Dark => Theme::Dark,
            Scheme::Light => Theme::Light,
        };
        let mut v = match self.scheme {
            Scheme::Dark => Visuals::dark(),
            Scheme::Light => Visuals::light(),
        };

        v.panel_fill = self.bg;
        v.window_fill = self.header;
        v.window_stroke = Stroke::new(1.0, self.border);
        v.extreme_bg_color = self.field;
        v.faint_bg_color = self.terminal_bar;
        v.code_bg_color = self.terminal_bar;
        v.hyperlink_color = self.accent_text;
        v.override_text_color = Some(self.text);
        v.selection.bg_fill = mix(self.field, self.accent, 0.45);
        v.selection.stroke = Stroke::new(1.0, self.text);
        v.text_cursor.stroke = Stroke::new(2.0, self.accent_text);

        let radius = CornerRadius::same(6);
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = self.bg;
        w.noninteractive.weak_bg_fill = self.bg;
        w.noninteractive.bg_stroke = Stroke::new(1.0, self.border);
        w.noninteractive.fg_stroke = Stroke::new(1.0, self.text);

        w.inactive.bg_fill = self.field;
        w.inactive.weak_bg_fill = self.field;
        w.inactive.bg_stroke = Stroke::new(1.0, self.border);
        w.inactive.fg_stroke = Stroke::new(1.0, self.text);

        let hover = mix(self.field, self.accent, 0.14);
        w.hovered.bg_fill = hover;
        w.hovered.weak_bg_fill = hover;
        w.hovered.bg_stroke = Stroke::new(1.0, mix(self.border, self.accent, 0.6));
        w.hovered.fg_stroke = Stroke::new(1.5, self.text);

        let active = mix(self.field, self.accent, 0.28);
        w.active.bg_fill = active;
        w.active.weak_bg_fill = active;
        w.active.bg_stroke = Stroke::new(1.0, self.accent);
        w.active.fg_stroke = Stroke::new(2.0, self.text);

        w.open.bg_fill = self.field;
        w.open.weak_bg_fill = self.field;
        w.open.bg_stroke = Stroke::new(1.0, self.accent);
        w.open.fg_stroke = Stroke::new(1.0, self.text);

        for wv in [
            &mut w.noninteractive,
            &mut w.inactive,
            &mut w.hovered,
            &mut w.active,
            &mut w.open,
        ] {
            wv.corner_radius = radius;
        }

        ctx.set_visuals_of(theme, v);
        ctx.set_theme(theme);
        ctx.style_mut_of(theme, |s| {
            s.spacing.item_spacing = egui::vec2(8.0, 6.0);
            s.spacing.button_padding = egui::vec2(10.0, 5.0);
            s.spacing.interact_size.y = 28.0;
        });
    }
}

// ─────────────────────────── Utilidades ───────────────────────────

pub const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

/// Mistura `a` com `b` (`t` = 0 → só `a`, 1 → só `b`).
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// Luminância relativa (WCAG 2).
fn luminance(c: Color32) -> f32 {
    let ch = |v: u8| {
        let s = v as f32 / 255.0;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * ch(c.r()) + 0.7152 * ch(c.g()) + 0.0722 * ch(c.b())
}

/// Razão de contraste WCAG entre duas cores (1.0 a 21.0).
pub fn contrast(a: Color32, b: Color32) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// Clareia/escurece `fg` aos poucos até atingir o contraste mínimo sobre `bg`.
pub fn ensure_contrast(fg: Color32, bg: Color32, min: f32) -> Color32 {
    let target = if luminance(bg) < 0.4 {
        Color32::WHITE
    } else {
        Color32::BLACK
    };
    let mut t: f32 = 0.0;
    let mut c = fg;
    while contrast(c, bg) < min && t < 1.0 {
        t += 0.04;
        c = mix(fg, target, t.min(1.0));
    }
    c
}

/// Preto ou branco: o que tiver mais contraste sobre `bg`.
fn best_on(bg: Color32) -> Color32 {
    let dark = Color32::BLACK; // preto puro garante >= 4.58:1 com preto OU branco
    if contrast(dark, bg) >= contrast(Color32::WHITE, bg) {
        dark
    } else {
        Color32::WHITE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cores de destaque do GNOME + Windows padrão + extremos.
    const ACCENTS: &[u32] = &[
        0x3584E4, 0x2190A4, 0x3A944A, 0xC88800, 0xED5B00, 0xE62D42, 0xD56199, 0x9141AC, 0x6F8396,
        0x0078D7, 0xFFFF00, 0x000080, 0xFFFFFF, 0x000000,
    ];

    #[test]
    fn contraste_basico() {
        assert!((contrast(Color32::BLACK, Color32::WHITE) - 21.0).abs() < 0.01);
        assert!((contrast(Color32::WHITE, Color32::WHITE) - 1.0).abs() < 0.01);
    }

    #[test]
    fn paleta_sempre_legivel() {
        for &a in ACCENTS {
            for scheme in [Scheme::Dark, Scheme::Light] {
                let p = Palette::new(scheme, Some(hex(a)));
                let ctx = format!("accent {a:06X} {scheme:?}");
                assert!(contrast(p.text, p.terminal) >= 7.0, "texto: {ctx}");
                assert!(contrast(p.muted, p.header) >= 4.5, "muted: {ctx}");
                assert!(contrast(p.faint, p.terminal) >= 4.5, "faint: {ctx}");
                assert!(
                    contrast(p.accent_text, p.terminal) >= 4.5,
                    "accent_text: {ctx}"
                );
                assert!(contrast(p.on_accent, p.accent) >= 4.5, "on_accent: {ctx}");
                assert!(contrast(p.ok_text, p.ok_bg) >= 4.5, "ok: {ctx}");
            }
        }
    }

    #[test]
    fn sem_destaque_usa_padrao() {
        let p = Palette::new(Scheme::Dark, None);
        assert_eq!(p.accent, FALLBACK_ACCENT);
        assert!(!p.accent_from_system);
    }
}
