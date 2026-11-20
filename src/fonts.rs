//! Fontes do sistema: usa a fonte de interface e a monoespaçada escolhidas
//! pelo usuário (GNOME/KDE) ou as padrão do sistema (Windows, macOS, fontconfig).
//!
//! As fontes embutidas do egui continuam como reserva para símbolos e emojis.

use eframe::egui::{
    self, FontData, FontFamily, TextStyle,
    epaint::text::{FontInsert, FontPriority, InsertFontFamily},
};
use fontique::{
    Attributes, Collection, CollectionOptions, GenericFamily, QueryFamily, QueryStatus,
    SourceCache, SourceCacheOptions,
};

use crate::system::FontSpec;

/// Tamanhos (px) quando o sistema não informa um.
const DEFAULT_UI_PX: f32 = 13.0;
const DEFAULT_MONO_PX: f32 = 13.0;

/// Uma fonte encontrada no disco, pronta para o egui.
pub struct LoadedFont {
    pub family: String,
    data: FontData,
}

/// Resultado da busca (feita fora da thread da interface).
pub struct SystemFonts {
    pub ui: Option<LoadedFont>,
    pub mono: Option<LoadedFont>,
    pub ui_px: f32,
    pub mono_px: f32,
}

impl Default for SystemFonts {
    /// Sem fontes do sistema: ficam as embutidas do egui.
    fn default() -> Self {
        Self {
            ui: None,
            mono: None,
            ui_px: DEFAULT_UI_PX,
            mono_px: DEFAULT_MONO_PX,
        }
    }
}

/// Procura as fontes no sistema. Pode levar algumas centenas de ms
/// (enumera as fontes instaladas), por isso roda em outra thread no `main`.
pub fn load(ui: Option<&FontSpec>, mono: Option<&FontSpec>) -> SystemFonts {
    let mut collection = Collection::new(CollectionOptions {
        shared: false,
        system_fonts: true,
    });
    let mut cache = SourceCache::new(SourceCacheOptions::default());

    let ui_font = find(
        &mut collection,
        &mut cache,
        ui.map(|s| s.family.as_str()),
        &[
            GenericFamily::SystemUi,
            GenericFamily::UiSansSerif,
            GenericFamily::SansSerif,
        ],
    );
    let mono_font = find(
        &mut collection,
        &mut cache,
        mono.map(|s| s.family.as_str()),
        &[GenericFamily::UiMonospace, GenericFamily::Monospace],
    );

    SystemFonts {
        ui: ui_font,
        mono: mono_font,
        ui_px: to_px(ui, DEFAULT_UI_PX),
        mono_px: to_px(mono, DEFAULT_MONO_PX),
    }
}

/// pt → px (1 pt = 4/3 px), limitado para não quebrar o layout.
fn to_px(spec: Option<&FontSpec>, default: f32) -> f32 {
    spec.and_then(|s| s.size_pt)
        .map_or(default, |pt| (pt * 4.0 / 3.0).clamp(11.0, 17.0))
}

/// Primeira fonte (peso normal) da família pedida ou, se não houver, das genéricas.
fn find(
    collection: &mut Collection,
    cache: &mut SourceCache,
    named: Option<&str>,
    generics: &[GenericFamily],
) -> Option<LoadedFont> {
    let mut families: Vec<QueryFamily<'_>> = named.map(QueryFamily::Named).into_iter().collect();
    families.extend(generics.iter().map(|g| QueryFamily::Generic(*g)));

    let mut found = None;
    {
        let mut query = collection.query(cache);
        query.set_families(families);
        query.set_attributes(Attributes::default());
        query.matches_with(|font| {
            // Precisa ter as letras básicas (descarta fontes de símbolos).
            let latin = font
                .charmap()
                .is_some_and(|c| "Aa0ç".chars().all(|ch| c.map(ch).is_some()));
            if latin {
                found = Some(font.clone());
                QueryStatus::Stop
            } else {
                QueryStatus::Continue
            }
        });
    }
    let font = found?;
    let family = collection
        .family_name(font.family.0)
        .unwrap_or("?")
        .to_string();
    let (blob, _) = font.blob.into_raw_parts();
    Some(LoadedFont {
        family,
        data: FontData::from_blob(blob, font.index),
    })
}

/// Instala as fontes no egui (prioridade máxima) e ajusta os tamanhos de texto.
pub fn apply(ctx: &egui::Context, fonts: SystemFonts) {
    for (font, family) in [
        (fonts.ui, FontFamily::Proportional),
        (fonts.mono, FontFamily::Monospace),
    ] {
        if let Some(f) = font {
            ctx.add_font(FontInsert {
                name: format!("system:{}", f.family),
                data: f.data,
                families: vec![InsertFontFamily {
                    family,
                    priority: FontPriority::Highest,
                }],
            });
        }
    }

    let (ui, mono) = (fonts.ui_px, fonts.mono_px);
    ctx.all_styles_mut(|style| {
        use egui::FontId;
        style.text_styles = [
            (TextStyle::Small, FontId::proportional((ui * 0.88).round())),
            (TextStyle::Body, FontId::proportional(ui)),
            (TextStyle::Button, FontId::proportional(ui)),
            (
                TextStyle::Heading,
                FontId::proportional((ui * 1.15).round()),
            ),
            (TextStyle::Monospace, FontId::monospace(mono)),
        ]
        .into();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(size: f32) -> FontSpec {
        FontSpec {
            family: "X".into(),
            size_pt: Some(size),
        }
    }

    #[test]
    fn to_px_converte_pontos_em_pixels() {
        assert_eq!(to_px(Some(&pt(9.0)), 13.0), 12.0);
    }

    #[test]
    fn to_px_limita_fontes_enormes_para_nao_quebrar_o_layout() {
        assert_eq!(to_px(Some(&pt(30.0)), 13.0), 17.0);
    }

    #[test]
    fn to_px_usa_padrao_sem_tamanho_do_sistema() {
        assert_eq!(to_px(None, 13.0), 13.0);
    }
}
