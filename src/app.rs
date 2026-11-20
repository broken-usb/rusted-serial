//! Interface gráfica (egui/eframe).

use eframe::egui;

use crate::serial::{Connection, DataBits, Event, FlowControl, Parity, PortConfig, StopBits};

const BAUD_RATES: &[u32] = &[
    300, 1200, 2400, 4800, 9600, 19200, 38400, 57600, 115200, 230400, 460800, 921600,
];
/// Limite do buffer do terminal (evita consumir memória infinita).
const MAX_BUFFER: usize = 1024 * 1024;

#[derive(PartialEq, Clone, Copy)]
enum LineEnding {
    None,
    Cr,
    Lf,
    CrLf,
}

impl LineEnding {
    fn bytes(self) -> &'static [u8] {
        match self {
            Self::None => b"",
            Self::Cr => b"\r",
            Self::Lf => b"\n",
            Self::CrLf => b"\r\n",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::None => "Nenhum",
            Self::Cr => "CR (\\r)",
            Self::Lf => "LF (\\n)",
            Self::CrLf => "CR+LF",
        }
    }
}

pub struct SerialApp {
    // Configuração
    ports: Vec<String>,
    config: PortConfig,
    line_ending: LineEnding,

    // Conexão
    conn: Option<Connection>,
    status: String,

    // Terminal
    rx_bytes: Vec<u8>,
    text_view: String,
    hex_view: String,
    hex_col: usize,
    show_hex: bool,
    autoscroll: bool,

    // Envio
    input: String,
    send_as_hex: bool,
    history: Vec<String>,
}

impl SerialApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let ports = crate::serial::list_ports();
        let mut config = PortConfig::default();
        if let Some(first) = ports.first() {
            config.name = first.clone();
        }
        Self {
            ports,
            config,
            line_ending: LineEnding::CrLf,
            conn: None,
            status: "Desconectado".into(),
            rx_bytes: Vec::new(),
            text_view: String::new(),
            hex_view: String::new(),
            hex_col: 0,
            show_hex: false,
            autoscroll: true,
            input: String::new(),
            send_as_hex: false,
            history: Vec::new(),
        }
    }

    fn connect(&mut self, ctx: &egui::Context) {
        let ctx = ctx.clone();
        match Connection::open(self.config.clone(), move || ctx.request_repaint()) {
            Ok(c) => {
                self.status = format!("Conectado em {} @ {}", c.config.name, c.config.baud_rate);
                self.conn = Some(c);
            }
            Err(e) => self.status = format!("Falha ao abrir {}: {e}", self.config.name),
        }
    }

    fn disconnect(&mut self) {
        self.conn = None; // Drop encerra a thread
        self.status = "Desconectado".into();
    }

    fn clear(&mut self) {
        self.rx_bytes.clear();
        self.text_view.clear();
        self.hex_view.clear();
        self.hex_col = 0;
    }

    /// Acrescenta bytes recebidos às visões texto e hex de forma incremental
    /// (não reprocessa o buffer inteiro a cada frame).
    fn push_rx(&mut self, data: &[u8]) {
        if self.rx_bytes.len() + data.len() > MAX_BUFFER {
            self.clear();
        }
        self.rx_bytes.extend_from_slice(data);

        // Texto: ignora '\r' (switches mandam \r\n) e troca bytes de controle por '·'
        for ch in String::from_utf8_lossy(data).chars() {
            match ch {
                '\r' => {}
                '\n' | '\t' => self.text_view.push(ch),
                c if c.is_control() => self.text_view.push('·'),
                c => self.text_view.push(c),
            }
        }

        // Hex: 16 bytes por linha
        for b in data {
            use std::fmt::Write;
            let _ = write!(self.hex_view, "{b:02X} ");
            self.hex_col += 1;
            if self.hex_col == 16 {
                self.hex_view.push('\n');
                self.hex_col = 0;
            }
        }
    }

    fn send_input(&mut self) {
        let Some(conn) = &self.conn else { return };
        let mut bytes = if self.send_as_hex {
            match parse_hex(&self.input) {
                Ok(b) => b,
                Err(e) => {
                    self.status = e;
                    return;
                }
            }
        } else {
            self.input.as_bytes().to_vec()
        };
        bytes.extend_from_slice(self.line_ending.bytes());
        conn.send(bytes);

        if !self.input.is_empty() && self.history.last() != Some(&self.input) {
            self.history.push(self.input.clone());
        }
        self.input.clear();
    }

    fn process_events(&mut self) {
        let Some(conn) = &self.conn else { return };
        let mut lost = false;
        for ev in conn.poll() {
            match ev {
                Event::Data(d) => self.push_rx(&d),
                Event::Error(e) => self.status = e,
                Event::Disconnected => lost = true,
            }
        }
        if lost {
            self.conn = None;
            if !self.status.starts_with("Erro") {
                self.status = "Desconectado".into();
            }
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let connected = self.conn.is_some();
        ui.horizontal_wrapped(|ui| {
            ui.add_enabled_ui(!connected, |ui| {
                let cfg = &mut self.config;

                ui.label("Porta");
                egui::ComboBox::from_id_salt("port")
                    .width(150.0)
                    .selected_text(if cfg.name.is_empty() {
                        "—"
                    } else {
                        cfg.name.as_str()
                    })
                    .show_ui(ui, |ui| {
                        for p in &self.ports {
                            ui.selectable_value(&mut cfg.name, p.clone(), p);
                        }
                    });
                if ui.button("⟳").on_hover_text("Atualizar portas").clicked() {
                    self.ports = crate::serial::list_ports();
                }

                ui.label("Baud");
                combo(ui, "baud", &mut cfg.baud_rate, BAUD_RATES, |b| {
                    b.to_string()
                });

                ui.label("Dados");
                combo(
                    ui,
                    "data",
                    &mut cfg.data_bits,
                    &[
                        DataBits::Five,
                        DataBits::Six,
                        DataBits::Seven,
                        DataBits::Eight,
                    ],
                    |d| u8::from(*d).to_string(),
                );

                ui.label("Paridade");
                combo(
                    ui,
                    "parity",
                    &mut cfg.parity,
                    &[Parity::None, Parity::Odd, Parity::Even],
                    |p| {
                        match p {
                            Parity::None => "Nenhuma",
                            Parity::Odd => "Ímpar",
                            Parity::Even => "Par",
                        }
                        .into()
                    },
                );

                ui.label("Stop");
                combo(
                    ui,
                    "stop",
                    &mut cfg.stop_bits,
                    &[StopBits::One, StopBits::Two],
                    |s| u8::from(*s).to_string(),
                );

                ui.label("Fluxo");
                combo(
                    ui,
                    "flow",
                    &mut cfg.flow_control,
                    &[
                        FlowControl::None,
                        FlowControl::Software,
                        FlowControl::Hardware,
                    ],
                    |f| {
                        match f {
                            FlowControl::None => "Nenhum",
                            FlowControl::Software => "XON/XOFF",
                            FlowControl::Hardware => "RTS/CTS",
                        }
                        .into()
                    },
                );
            });

            ui.separator();
            if connected {
                if ui.button("⏹ Desconectar").clicked() {
                    self.disconnect();
                }
            } else {
                let can = !self.config.name.is_empty();
                if ui
                    .add_enabled(can, egui::Button::new("▶ Conectar"))
                    .clicked()
                {
                    self.connect(ui.ctx());
                }
            }
        });
    }
}

/// ComboBox genérico: evita repetir o mesmo código para cada parâmetro.
fn combo<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    id: &str,
    value: &mut T,
    options: &[T],
    label: impl Fn(&T) -> String,
) {
    egui::ComboBox::from_id_salt(id)
        .width(90.0)
        .selected_text(label(value))
        .show_ui(ui, |ui| {
            for opt in options {
                ui.selectable_value(value, *opt, label(opt));
            }
        });
}

impl eframe::App for SerialApp {
    /// Lógica sem desenho: roda antes de cada frame (e também com a janela minimizada).
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.process_events();
    }

    /// Desenho da interface.
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.add_space(4.0);
            self.toolbar(ui);
            ui.add_space(4.0);
        });

        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(&self.status);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(format!("RX: {} bytes", self.rx_bytes.len()));
                });
            });
        });

        egui::Panel::bottom("input").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let resp = ui.add_enabled(
                    self.conn.is_some(),
                    egui::TextEdit::singleline(&mut self.input)
                        .hint_text(if self.send_as_hex {
                            "Ex.: AA 01 FF"
                        } else {
                            "Digite e pressione Enter"
                        })
                        .font(egui::TextStyle::Monospace)
                        .desired_width(ui.available_width() - 330.0),
                );
                // ↑ recupera o último comando
                if resp.has_focus()
                    && ui.input(|i| i.key_pressed(egui::Key::ArrowUp))
                    && let Some(last) = self.history.last()
                {
                    self.input = last.clone();
                }
                let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if enter {
                    self.send_input();
                    resp.request_focus();
                }
                if ui
                    .add_enabled(self.conn.is_some(), egui::Button::new("Enviar"))
                    .clicked()
                {
                    self.send_input();
                }
                egui::ComboBox::from_id_salt("eol")
                    .selected_text(self.line_ending.label())
                    .show_ui(ui, |ui| {
                        for le in [
                            LineEnding::None,
                            LineEnding::Cr,
                            LineEnding::Lf,
                            LineEnding::CrLf,
                        ] {
                            ui.selectable_value(&mut self.line_ending, le, le.label());
                        }
                    });
                ui.checkbox(&mut self.send_as_hex, "HEX");
            });
            ui.add_space(4.0);
        });

        egui::CentralPanel::default().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.show_hex, "Ver em HEX");
                ui.checkbox(&mut self.autoscroll, "Auto-scroll");
                if ui.button("Limpar").clicked() {
                    self.clear();
                }
            });
            ui.separator();

            let text = if self.show_hex {
                &self.hex_view
            } else {
                &self.text_view
            };
            egui::ScrollArea::both()
                .auto_shrink([false, false])
                .stick_to_bottom(self.autoscroll)
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(egui::RichText::new(text.as_str()).monospace())
                            .wrap_mode(egui::TextWrapMode::Extend),
                    );
                });
        });
    }
}

/// Converte "AA 01 ff" ou "AA01FF" em bytes.
fn parse_hex(s: &str) -> Result<Vec<u8>, String> {
    let clean: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if !clean.len().is_multiple_of(2) {
        return Err("HEX inválido: número ímpar de dígitos".into());
    }
    (0..clean.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&clean[i..i + 2], 16)
                .map_err(|_| format!("HEX inválido: {}", &clean[i..i + 2]))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::parse_hex;

    #[test]
    fn hex_ok() {
        assert_eq!(parse_hex("AA 01 ff").unwrap(), vec![0xAA, 0x01, 0xFF]);
        assert_eq!(parse_hex("0a0D").unwrap(), vec![0x0A, 0x0D]);
    }

    #[test]
    fn hex_err() {
        assert!(parse_hex("ABC").is_err());
        assert!(parse_hex("ZZ").is_err());
    }
}
