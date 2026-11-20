//! Rusted Serial: terminal serial nativo para Arduino, microcontroladores
//! e consoles de switches gerenciáveis.
//!
//! - [`serial`]: conexão com a porta numa thread própria (nunca bloqueia a interface).
//! - [`app`]: interface em egui.
//! - [`system`], [`theme`], [`fonts`]: leem o tema, a cor de destaque e as fontes
//!   do sistema uma vez, ao iniciar.

// No Windows, não abre uma janela de console junto com o app (só no build release).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod fonts;
mod icons;
mod serial;
mod system;
mod theme;

/// Ícone da janela ("Laptop", por Delapouite, game-icons.net, CC BY 3.0).
const ICON_PNG: &[u8] = include_bytes!("../assets/icon.png");

fn main() -> eframe::Result {
    // Lê tema e fontes do sistema em paralelo com a abertura da janela.
    let startup = std::thread::spawn(|| {
        let prefs = system::read();
        let fonts = fonts::load(prefs.ui_font.as_ref(), prefs.mono_font.as_ref());
        (prefs, fonts)
    });

    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_title("Rusted Serial")
        .with_app_id("rusted-serial")
        .with_inner_size([1180.0, 720.0])
        .with_min_inner_size([980.0, 520.0]);
    if let Ok(icon) = eframe::icon_data::from_png_bytes(ICON_PNG) {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "Rusted Serial",
        options,
        Box::new(move |cc| {
            let (prefs, fonts) = startup.join().unwrap_or_default();
            Ok(Box::new(app::SerialApp::new(cc, &prefs, fonts)))
        }),
    )
}
