//! Preferências do sistema operacional, lidas UMA vez ao iniciar o app.
//!
//! - Linux: portal `org.freedesktop.portal.Settings` via D-Bus (claro/escuro e
//!   cor de destaque). Funciona onde o portal do ambiente responde essas
//!   chaves: GNOME, KDE, COSMIC (`xdg-desktop-portal-cosmic`) e Cinnamon
//!   (`xdg-desktop-portal-xapp` 1.1.1 ou mais novo). As fontes vêm de onde cada
//!   ambiente as guarda (veja [`Desktop`]).
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
    use super::{
        Desktop, FontSpec, Scheme, SystemPrefs, detect_desktop, parse_cosmic_font,
        parse_gnome_font, parse_gvariant_string, parse_kde_font,
    };
    use eframe::egui::Color32;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use zbus::blocking::Connection;
    use zbus::zvariant::{OwnedValue, Value};

    const DEST: &str = "org.freedesktop.portal.Desktop";
    const PATH: &str = "/org/freedesktop/portal/desktop";
    const IFACE: &str = "org.freedesktop.portal.Settings";

    const APPEARANCE: &str = "org.freedesktop.appearance";
    const GNOME: &str = "org.gnome.desktop.interface";
    const KDE: &str = "org.kde.kdeglobals.General";
    const CINNAMON: &str = "org.cinnamon.desktop.interface";

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

        // Fontes: GNOME ("Adwaita Sans 11") ou KDE ("Noto Sans,10,-1,5,400,...").
        // O portal GTK responde as chaves do GNOME em qualquer ambiente (com o
        // padrão do GNOME se ninguém mexeu), então a ordem depende do ambiente.
        let gnome = |key: &str| string(GNOME, key).and_then(|s| parse_gnome_font(&s));
        let kde = |key: &str| string(KDE, key).and_then(|s| parse_kde_font(&s));
        let desktop = detect_desktop(&std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default());
        let (ui_font, mono_font) = match desktop {
            // O KDE copia a fonte geral para o GNOME, mas não a monoespaçada.
            Desktop::Kde => (
                kde("font").or_else(|| gnome("font-name")),
                kde("fixed").or_else(|| gnome("monospace-font-name")),
            ),
            // A fonte da interface fica no esquema do Cinnamon, que nenhum
            // portal expõe; a monoespaçada fica no do GNOME.
            Desktop::Cinnamon => (
                gsettings(CINNAMON, "font-name")
                    .and_then(|s| parse_gnome_font(&s))
                    .or_else(|| gnome("font-name")),
                gnome("monospace-font-name"),
            ),
            Desktop::Cosmic => cosmic_fonts(scheme),
            Desktop::Other => (
                gnome("font-name").or_else(|| kde("font")),
                gnome("monospace-font-name").or_else(|| kde("fixed")),
            ),
        };

        SystemPrefs {
            scheme,
            accent,
            ui_font,
            mono_font,
        }
        // `conn` é descartada aqui: nada continua rodando em segundo plano.
    }

    /// `gsettings get` (o dconf não tem API de leitura no D-Bus).
    fn gsettings(schema: &str, key: &str) -> Option<String> {
        let out = Command::new("gsettings")
            .args(["get", schema, key])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())?;
        parse_gvariant_string(String::from_utf8(out.stdout).ok()?.trim())
    }

    /// COSMIC guarda as fontes em arquivos RON próprios:
    /// - Epoch 1: `com.system76.CosmicTk/v1/{interface_font,monospace_font}`;
    /// - versões seguintes: `com.system76.CosmicTheme.{Dark,Light}/v1/{font,mono_font}`.
    ///
    /// Primeiro em `~/.config/cosmic`, depois nos padrões do sistema
    /// (`/usr/share/cosmic`). Sem nenhum arquivo, valem os padrões do COSMIC.
    /// Ele não tem ajuste de tamanho de fonte.
    fn cosmic_fonts(scheme: Option<Scheme>) -> (Option<FontSpec>, Option<FontSpec>) {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| home.map(|h| h.join(".config")));
        let data_dirs = std::env::var_os("XDG_DATA_DIRS")
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
        let roots: Vec<PathBuf> = config
            .into_iter()
            .chain(std::env::split_paths(&data_dirs))
            .filter(|p| p.is_absolute())
            .map(|p| p.join("cosmic"))
            .collect();
        let theme = match scheme {
            Some(Scheme::Light) => "com.system76.CosmicTheme.Light",
            _ => "com.system76.CosmicTheme.Dark",
        };
        let find = |tk_key: &str, theme_key: &str, default: &str| -> Option<FontSpec> {
            let family = roots
                .iter()
                .flat_map(|root| {
                    [
                        root.join("com.system76.CosmicTk/v1").join(tk_key),
                        root.join(theme).join("v1").join(theme_key),
                    ]
                })
                .find_map(|path| parse_cosmic_font(&std::fs::read_to_string(path).ok()?))
                .unwrap_or_else(|| default.to_string());
            Some(FontSpec {
                family,
                size_pt: None,
            })
        };
        (
            find("interface_font", "font", "Open Sans"),
            find("monospace_font", "mono_font", "Noto Sans Mono"),
        )
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

/// Ambiente gráfico, pelo `XDG_CURRENT_DESKTOP` ("KDE", "X-Cinnamon",
/// "COSMIC", "ubuntu:GNOME"...). Decide de onde vêm as fontes.
#[cfg(any(target_os = "linux", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Desktop {
    Kde,
    Cinnamon,
    Cosmic,
    /// GNOME e os demais: chaves do GNOME, depois as do KDE.
    Other,
}

#[cfg(any(target_os = "linux", test))]
pub fn detect_desktop(value: &str) -> Desktop {
    value
        .split(':')
        .find_map(|name| match name.trim().to_ascii_lowercase().as_str() {
            "kde" => Some(Desktop::Kde),
            "x-cinnamon" | "cinnamon" => Some(Desktop::Cinnamon),
            "cosmic" => Some(Desktop::Cosmic),
            _ => None,
        })
        .unwrap_or(Desktop::Other)
}

/// Saída do `gsettings get`: `'Ubuntu 10'` (ou `"..."` se o texto tiver `'`).
#[cfg(any(target_os = "linux", test))]
pub fn parse_gvariant_string(s: &str) -> Option<String> {
    let s = s.strip_prefix("@s ").unwrap_or(s);
    let quote = s.chars().next().filter(|c| matches!(c, '\'' | '"'))?;
    let inner = s.strip_prefix(quote)?.strip_suffix(quote)?;
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        out.push(if c == '\\' { chars.next()? } else { c });
    }
    (!out.trim().is_empty()).then_some(out)
}

/// Arquivo RON do COSMIC: `(family: "Open Sans", weight: Normal, ...)`.
#[cfg(any(target_os = "linux", test))]
pub fn parse_cosmic_font(ron: &str) -> Option<String> {
    let rest = &ron[ron.find("family")? + "family".len()..];
    let rest = rest.trim_start().strip_prefix(':')?.trim_start();
    let rest = rest
        .strip_prefix("Some(")
        .unwrap_or(rest)
        .strip_prefix('"')?;
    let family = rest[..rest.find('"')?].trim();
    (!family.is_empty()).then(|| family.to_string())
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

    mod detect_desktop {
        use super::*;

        #[test]
        fn reconhece_os_ambientes() {
            assert_eq!(detect_desktop("KDE"), Desktop::Kde);
            assert_eq!(detect_desktop("X-Cinnamon"), Desktop::Cinnamon);
            assert_eq!(detect_desktop("COSMIC"), Desktop::Cosmic);
            assert_eq!(detect_desktop("ubuntu:GNOME"), Desktop::Other);
            assert_eq!(detect_desktop(""), Desktop::Other);
        }

        #[test]
        fn olha_todos_os_nomes_da_lista() {
            assert_eq!(detect_desktop("pop:COSMIC"), Desktop::Cosmic);
        }
    }

    mod parse_gvariant_string {
        use super::*;

        #[test]
        fn tira_as_aspas() {
            assert_eq!(
                parse_gvariant_string("'Ubuntu 10'").as_deref(),
                Some("Ubuntu 10")
            );
        }

        #[test]
        fn aceita_aspas_duplas_e_escapes() {
            assert_eq!(
                parse_gvariant_string(r#""Joe's Sans 9""#).as_deref(),
                Some("Joe's Sans 9")
            );
            assert_eq!(parse_gvariant_string(r"'A\'B 9'").as_deref(), Some("A'B 9"));
        }

        #[test]
        fn recusa_o_que_nao_e_texto() {
            assert_eq!(parse_gvariant_string("10"), None);
            assert_eq!(parse_gvariant_string("''"), None);
            assert_eq!(parse_gvariant_string("'sem fim"), None);
        }
    }

    mod parse_cosmic_font {
        use super::*;

        #[test]
        fn le_a_familia() {
            let ron = "(\n    family: \"Fira Sans\",\n    weight: Normal,\n    stretch: Normal,\n    style: Normal,\n)";
            assert_eq!(parse_cosmic_font(ron).as_deref(), Some("Fira Sans"));
        }

        #[test]
        fn arquivo_sem_familia_nao_e_fonte() {
            assert_eq!(parse_cosmic_font("(weight: Bold)"), None);
            assert_eq!(parse_cosmic_font("(family: \"\")"), None);
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
