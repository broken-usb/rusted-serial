//! Ícones vetoriais desenhados com o `Painter` do egui (estilo contorno, traço uniforme).
//!
//! Substituem emojis e símbolos de fonte (💾 🗑 ⟳ ▶ ■ →), que mudam de aparência
//! conforme a fonte do sistema e não acompanham a paleta.

use eframe::egui::{
    self, Color32, CornerRadius, Painter, Rect, Response, Role, Sense, Shape, Stroke, StrokeKind,
    TextStyle, Ui, WidgetInfo, pos2, vec2,
};

use crate::theme::{Palette, mix};

#[derive(Clone, Copy, Debug)]
pub enum Icon {
    Refresh,
    Play,
    Stop,
    Save,
    Trash,
    Send,
    Lock,
}

/// Aparência do botão.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Ação principal: fundo na cor de destaque.
    Accent,
    /// Ação secundária: só contorno.
    Outline,
    /// Ação discreta em barra de ferramentas: sem fundo até passar o mouse.
    Plain,
}

/// Desenha `icon` dentro de `rect` (pensado numa grade de 16 × 16).
pub fn paint(painter: &Painter, icon: Icon, rect: Rect, color: Color32) {
    let s = rect.width() / 16.0;
    let p = |x: f32, y: f32| rect.min + vec2(x * s, y * s);
    let stroke = Stroke::new((1.6 * s).max(1.2), color);
    let line = |pts: &[(f32, f32)]| {
        painter.add(Shape::line(
            pts.iter().map(|&(x, y)| p(x, y)).collect(),
            stroke,
        ));
    };

    match icon {
        Icon::Play => {
            let pts = vec![p(5.0, 3.0), p(13.0, 8.0), p(5.0, 13.0)];
            painter.add(Shape::convex_polygon(pts, color, Stroke::NONE));
        }
        Icon::Stop => {
            let r = Rect::from_min_max(p(4.0, 4.0), p(12.0, 12.0));
            painter.rect_filled(r, CornerRadius::same((1.5 * s) as u8), color);
        }
        Icon::Send => {
            line(&[(2.5, 8.0), (13.0, 8.0)]);
            line(&[(8.5, 3.5), (13.0, 8.0), (8.5, 12.5)]);
        }
        Icon::Refresh => {
            // Arco de ~290° com a ponta da seta no fim.
            let c = (8.0, 8.0);
            let r = 5.2;
            let arc: Vec<(f32, f32)> = (0..=24)
                .map(|i| {
                    let a = (-50.0 + 290.0 * i as f32 / 24.0).to_radians();
                    (c.0 + r * a.cos(), c.1 + r * a.sin())
                })
                .collect();
            line(&arc);
            let (ex, ey) = (
                c.0 + r * (-50f32).to_radians().cos(),
                c.1 + r * (-50f32).to_radians().sin(),
            );
            line(&[(ex - 3.4, ey - 0.4), (ex, ey), (ex + 0.4, ey + 3.4)]);
        }
        Icon::Save => {
            let body = Rect::from_min_max(p(2.5, 2.5), p(13.5, 13.5));
            painter.rect_stroke(
                body,
                CornerRadius::same((1.5 * s) as u8),
                stroke,
                StrokeKind::Middle,
            );
            line(&[(5.0, 2.5), (5.0, 6.0), (11.0, 6.0), (11.0, 2.5)]);
            line(&[(4.8, 13.5), (4.8, 9.5), (11.2, 9.5), (11.2, 13.5)]);
        }
        Icon::Trash => {
            line(&[(2.5, 4.5), (13.5, 4.5)]);
            line(&[(6.0, 4.5), (6.0, 2.6), (10.0, 2.6), (10.0, 4.5)]);
            line(&[(4.0, 4.5), (4.8, 13.5), (11.2, 13.5), (12.0, 4.5)]);
            line(&[(6.8, 7.0), (6.8, 11.0)]);
            line(&[(9.2, 7.0), (9.2, 11.0)]);
        }
        Icon::Lock => {
            let body = Rect::from_min_max(p(3.5, 7.5), p(12.5, 13.5));
            painter.rect_stroke(
                body,
                CornerRadius::same((1.5 * s) as u8),
                stroke,
                StrokeKind::Middle,
            );
            let shackle: Vec<(f32, f32)> = (0..=12)
                .map(|i| {
                    let a = (180.0 + 180.0 * i as f32 / 12.0f32).to_radians();
                    (8.0 + 3.0 * a.cos(), 7.5 + 3.4 * a.sin())
                })
                .collect();
            line(&shackle);
        }
    }
}

/// Botão com ícone e (opcionalmente) texto.
///
/// `name` é o nome lido por leitores de tela; para botões só com ícone ele é
/// a única descrição do botão, então use algo como "Atualizar portas".
pub fn button(
    ui: &mut Ui,
    pal: &Palette,
    kind: Kind,
    icon: Icon,
    label: Option<&str>,
    name: &str,
) -> Response {
    let enabled = ui.is_enabled();
    let font = TextStyle::Button.resolve(ui.style());
    let icon_size = (font.size * 1.1).round();
    let galley = label.map(|t| ui.painter().layout_no_wrap(t.to_owned(), font, pal.text));

    let (pad_x, gap) = (10.0, 7.0);
    let text_w = galley.as_ref().map_or(0.0, |g| gap + g.size().x);
    let text_h = galley.as_ref().map_or(0.0, |g| g.size().y);
    let size = vec2(
        pad_x * 2.0 + icon_size + text_w,
        (icon_size.max(text_h) + 12.0).max(30.0),
    );

    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, name));
    if !ui.is_rect_visible(rect) {
        return resp;
    }

    let hovered = enabled && resp.hovered();
    let pressed = enabled && resp.is_pointer_button_down_on();
    let lift = if pressed {
        0.24
    } else if hovered {
        0.12
    } else {
        0.0
    };
    let (mut fill, stroke, mut fg) = match kind {
        Kind::Accent => (
            mix(pal.accent, pal.on_accent, lift),
            Stroke::NONE,
            pal.on_accent,
        ),
        Kind::Outline => (
            if lift > 0.0 {
                mix(pal.field, pal.accent, lift)
            } else {
                Color32::TRANSPARENT
            },
            Stroke::new(1.0, if hovered { pal.accent } else { pal.border }),
            pal.text,
        ),
        Kind::Plain => (
            if lift > 0.0 {
                mix(pal.field, pal.accent, lift)
            } else {
                Color32::TRANSPARENT
            },
            Stroke::NONE,
            pal.text,
        ),
    };
    if !enabled {
        fill = fill.gamma_multiply(0.45);
        fg = fg.gamma_multiply(0.45);
    }

    let painter = ui.painter();
    painter.rect(
        rect,
        CornerRadius::same(7),
        fill,
        stroke,
        StrokeKind::Inside,
    );
    let icon_rect = Rect::from_min_size(
        pos2(rect.left() + pad_x, rect.center().y - icon_size / 2.0),
        egui::Vec2::splat(icon_size),
    );
    paint(painter, icon, icon_rect, fg);
    if let Some(g) = galley {
        let pos = pos2(icon_rect.right() + gap, rect.center().y - g.size().y / 2.0);
        painter.galley_with_override_text_color(pos, g, fg);
    }
    // Anel de foco visível para quem navega com Tab.
    if resp.has_focus() {
        painter.rect_stroke(
            rect.expand(2.0),
            CornerRadius::same(9),
            Stroke::new(2.0, pal.accent_text),
            StrokeKind::Outside,
        );
    }
    resp
}
