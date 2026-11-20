//! Preferências do sistema operacional, lidas UMA vez ao iniciar o app.
//!
//! - Linux: portal `org.freedesktop.portal.Settings` via D-Bus
//!   (claro/escuro, cor de destaque e as fontes do GNOME ou do KDE).
//! - Windows: registro do usuário (claro/escuro e cor de destaque). As fontes
//!   do Windows são achadas pelo `fonts.rs` (fonte de interface do sistema).

use eframe::egui::Color32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
    Light,
    Dark,
}

/// Uma fonte escolhida pelo usuário nas configurações do sistema.
#[derive(Clone, Debug, PartialEq)]
pub struct FontSpec {
    pub family: String,
    /// Tamanho em pontos (pt), quando o sistema informa.
    pub size_pt: Option<f32>,
}

/// O que conseguimos ler do sistema (cada campo pode faltar).
#[derive(Clone, Debug, Default)]
pub struct SystemPrefs {
    pub scheme: Option<Scheme>,
    pub accent: Option<Color32>,
    pub ui_font: Option<FontSpec>,
    pub mono_font: Option<FontSpec>,
}

/// Consulta o sistema operacional. Não fica observando mudanças.
pub fn read() -> SystemPrefs {
    platform::read()
}

// ───────────────────────────── Linux ─────────────────────────────
#[cfg(target_os = "linux")]
mod platform {
    use super::{FontSpec, Scheme, SystemPrefs, parse_gnome_font, parse_kde_font};
    use eframe::egui::Color32;
    use zbus::blocking::Connection;
    use zbus::zvariant::{OwnedValue, Value};

    const DEST: &str = "org.freedesktop.portal.Desktop";
    const PATH: &str = "/org/freedesktop/portal/desktop";
    const IFACE: &str = "org.freedesktop.portal.Settings";

    const APPEARANCE: &str = "org.freedesktop.appearance";
    const GNOME: &str = "org.gnome.desktop.interface";
    const KDE: &str = "org.kde.kdeglobals.General";

    pub fn read() -> SystemPrefs {
        let Ok(conn) = Connection::session() else {
            return SystemPrefs::default();
        };
        let get = |ns: &str, key: &str| read_setting(&conn, ns, key);
        let string = |ns: &str, key: &str| get(ns, key).and_then(|v| String::try_from(v).ok());

        // color-scheme (u): 0 = sem preferência, 1 = escuro, 2 = claro
        let scheme = get(APPEARANCE, "color-scheme")
            .and_then(|v| u32::try_from(v).ok())
            .and_then(|n| match n {
                1 => Some(Scheme::Dark),
                2 => Some(Scheme::Light),
                _ => None,
            });

        // accent-color ((ddd)): RGB sRGB em [0,1]; fora da faixa = não definido
        let accent = get(APPEARANCE, "accent-color")
            .and_then(|v| <(f64, f64, f64)>::try_from(v).ok())
            .filter(|(r, g, b)| [r, g, b].iter().all(|c| (0.0..=1.0).contains(*c)))
            .map(|(r, g, b)| {
                let c = |x: f64| (x * 255.0).round() as u8;
                Color32::from_rgb(c(r), c(g), c(b))
            });

        // Fontes: GNOME ("Adwaita Sans 11") ou KDE ("Noto Sans,10,-1,5,400,...")
        let font = |gnome_key: &str, kde_key: &str| -> Option<FontSpec> {
            string(GNOME, gnome_key)
                .and_then(|s| parse_gnome_font(&s))
                .or_else(|| string(KDE, kde_key).and_then(|s| parse_kde_font(&s)))
        };
        let ui_font = font("font-name", "font");
        let mono_font = font("monospace-font-name", "fixed");

        SystemPrefs {
            scheme,
            accent,
            ui_font,
            mono_font,
        }
        // `conn` é descartada aqui: nada continua rodando em segundo plano.
    }

    fn read_setting(conn: &Connection, ns: &str, key: &str) -> Option<OwnedValue> {
        // ReadOne: portais mais novos (>= 1.17).
        match conn.call_method(Some(DEST), PATH, Some(IFACE), "ReadOne", &(ns, key)) {
            Ok(msg) => return msg.body().deserialize::<OwnedValue>().ok(),
            // Chave inexistente: não adianta tentar o método antigo.
            Err(zbus::Error::MethodError(name, _, _))
                if name.as_str() == "org.freedesktop.portal.Error.NotFound" =>
            {
                return None;
            }
            Err(_) => {}
        }
        // Read (método antigo): o valor vem embrulhado em uma variante extra.
        let msg = conn
            .call_method(Some(DEST), PATH, Some(IFACE), "Read", &(ns, key))
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

        SystemPrefs {
            scheme,
            accent,
            ui_font: None,
            mono_font: None,
        }
    }
}

// ───────────────────────── Outros sistemas ─────────────────────────
#[cfg(not(any(target_os = "linux", windows)))]
mod platform {
    pub fn read() -> super::SystemPrefs {
        super::SystemPrefs::default()
    }
}

// ─────────────────────── Leitura dos nomes ───────────────────────

// Os nomes de fonte só vêm do portal no Linux; no Windows/macOS estas funções
// não seriam usadas (e o aviso de código morto quebraria o CI com -D warnings).

/// Palavras de estilo que às vezes vêm junto do nome ("Cantarell Bold 11").
#[cfg(any(target_os = "linux", test))]
const STYLE_WORDS: &[&str] = &[
    "Regular",
    "Book",
    "Medium",
    "Semibold",
    "SemiBold",
    "Bold",
    "Light",
    "Thin",
    "Italic",
    "Oblique",
    "Condensed",
];

/// GNOME/Pango: "Adwaita Sans 11", "Noto Sans Mono Bold 10".
#[cfg(any(target_os = "linux", test))]
pub fn parse_gnome_font(s: &str) -> Option<FontSpec> {
    let mut words: Vec<&str> = s.split_whitespace().collect();
    let size_pt = words.last().and_then(|w| w.parse::<f32>().ok());
    if size_pt.is_some() {
        words.pop();
    }
    while words.len() > 1 && STYLE_WORDS.contains(words.last()?) {
        words.pop();
    }
    let family = words.join(" ");
    (!family.is_empty()).then_some(FontSpec { family, size_pt })
}

/// KDE/Qt: "Noto Sans,10,-1,5,400,0,0,0,0,0,0,0,0,0,0,1".
#[cfg(any(target_os = "linux", test))]
pub fn parse_kde_font(s: &str) -> Option<FontSpec> {
    let mut parts = s.split(',');
    let family = parts.next()?.trim().to_string();
    let size_pt = parts
        .next()
        .and_then(|p| p.trim().parse::<f32>().ok())
        .filter(|v| *v > 0.0);
    (!family.is_empty()).then_some(FontSpec { family, size_pt })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(family: &str, size_pt: Option<f32>) -> FontSpec {
        FontSpec {
            family: family.into(),
            size_pt,
        }
    }

    mod parse_gnome_font {
        use super::*;

        #[test]
        fn separa_familia_e_tamanho() {
            assert_eq!(
                parse_gnome_font("Adwaita Sans 11"),
                Some(spec("Adwaita Sans", Some(11.0)))
            );
        }

        #[test]
        fn aceita_tamanho_fracionado() {
            assert_eq!(
                parse_gnome_font("Adwaita Mono 10.5"),
                Some(spec("Adwaita Mono", Some(10.5)))
            );
        }

        #[test]
        fn descarta_palavra_de_estilo() {
            assert_eq!(
                parse_gnome_font("Cantarell Bold 11"),
                Some(spec("Cantarell", Some(11.0)))
            );
        }

        #[test]
        fn aceita_nome_sem_tamanho() {
            assert_eq!(parse_gnome_font("Inter"), Some(spec("Inter", None)));
        }

        #[test]
        fn texto_vazio_nao_e_fonte() {
            assert_eq!(parse_gnome_font(""), None);
        }
    }

    mod parse_kde_font {
        use super::*;

        #[test]
        fn separa_familia_e_tamanho() {
            let qt = "Noto Sans,10,-1,5,400,0,0,0,0,0,0,0,0,0,0,1";
            assert_eq!(parse_kde_font(qt), Some(spec("Noto Sans", Some(10.0))));
        }

        #[test]
        fn tamanho_em_pixels_fica_sem_tamanho() {
            // Qt usa -1 no tamanho em pontos quando a fonte foi definida em pixels.
            assert_eq!(
                parse_kde_font("Hack,-1,12,5,50,0,0,0,0,0"),
                Some(spec("Hack", None))
            );
        }

        #[test]
        fn texto_vazio_nao_e_fonte() {
            assert_eq!(parse_kde_font(""), None);
        }
    }
}
