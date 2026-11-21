//! Interface gráfica (egui/eframe) — layout "Clássico refinado".

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Color32, CornerRadius, FontId, Layout, Margin, RichText, Sense, Stroke,
    TextFormat, TextStyle, text::LayoutJob,
};

use crate::fonts::SystemFonts;
use crate::icons::{self, Icon, Kind};
use crate::serial::{Connection, DataBits, Event, FlowControl, Parity, PortConfig, StopBits};
use crate::system::SystemPrefs;
use crate::theme::{Palette, Scheme};

const BAUD_RATES: &[u32] = &[
    300, 1200, 2400, 4800, 9600, 19200, 38400, 57600, 115200, 230400, 460800, 921600,
];

/// Formatos de quadro mais usados: (rótulo, dados, paridade, stop).
const FORMATS: &[(&str, DataBits, Parity, StopBits)] = &[
    ("8N1", DataBits::Eight, Parity::None, StopBits::One),
    ("8N2", DataBits::Eight, Parity::None, StopBits::Two),
    ("8E1", DataBits::Eight, Parity::Even, StopBits::One),
    ("8O1", DataBits::Eight, Parity::Odd, StopBits::One),
    ("7E1", DataBits::Seven, Parity::Even, StopBits::One),
    ("7O1", DataBits::Seven, Parity::Odd, StopBits::One),
    ("7N1", DataBits::Seven, Parity::None, StopBits::One),
    ("7N2", DataBits::Seven, Parity::None, StopBits::Two),
];

const FLOWS: &[(&str, FlowControl)] = &[
    ("Nenhum", FlowControl::None),
    ("RTS/CTS", FlowControl::Hardware),
    ("XON/XOFF", FlowControl::Software),
];

/// Máximo de linhas guardadas no terminal.
const MAX_LINES: usize = 100_000;
/// Quebra linhas recebidas sem '\n' (ex.: protocolo binário) neste tamanho.
const MAX_LINE_BYTES: usize = 256;
/// Intervalo mínimo entre redesenhos causados por dados chegando (~30 quadros/s).
/// PERF: o custo de um fluxo contínuo é quase todo desenhar quadros; a 30/s o texto
/// ainda rola suave e a digitação não é afetada (teclado redesenha na hora).
const FRAME_INTERVAL: Duration = Duration::from_millis(33);
/// O que aparece no lugar de senhas, no terminal e no log (comprimento fixo:
/// não revela o tamanho da senha).
const SECRET_MASK: &str = "••••••••";

#[derive(PartialEq, Clone, Copy)]
enum LineEnding {
    None,
    Cr,
    Lf,
    CrLf,
}

impl LineEnding {
    const ALL: [Self; 4] = [Self::None, Self::Cr, Self::Lf, Self::CrLf];
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
            Self::Cr => "CR",
            Self::Lf => "LF",
            Self::CrLf => "CR+LF",
        }
    }
}

#[derive(PartialEq, Clone, Copy)]
enum Dir {
    Rx,
    Tx,
    Info,
    Error,
}

/// Uma linha do terminal.
struct Line {
    time: String,
    dir: Dir,
    bytes: Vec<u8>,
    /// Senha digitada: nunca é mostrada nem salva no log.
    masked: bool,
}

impl Line {
    fn new(dir: Dir, bytes: Vec<u8>) -> Self {
        Self {
            time: jiff::ZonedDateTime::now()
                .strftime("%H:%M:%S.%3f")
                .to_string(),
            dir,
            bytes,
            masked: false,
        }
    }
}

pub struct SerialApp {
    pal: Palette,
    /// Barra superior em duas linhas (fonte grande ou janela estreita).
    header_compact: bool,
    /// Fontes do sistema em uso (interface, monoespaçada), mostradas no rodapé.
    font_names: (Option<String>, Option<String>),

    // Configuração
    ports: Vec<String>,
    port: String,
    custom_port: String,
    baud: u32,
    format: usize,
    flow: usize,
    line_ending: LineEnding,

    // Conexão
    conn: Option<Connection>,
    connected_at: Option<Instant>,
    notice: Option<(String, bool)>, // (mensagem, é erro?)

    // Terminal
    lines: Vec<Line>,
    rx_open: bool,
    rx_total: u64,
    tx_total: u64,
    show_hex: bool,
    timestamps: bool,
    autoscroll: bool,

    // Envio
    input: String,
    /// Erro do campo de envio (ex.: HEX inválido), mostrado logo abaixo dele.
    input_error: Option<String>,
    send_as_hex: bool,
    history: Vec<String>,
    history_pos: Option<usize>,
}

impl SerialApp {
    pub fn new(cc: &eframe::CreationContext<'_>, sys: &SystemPrefs, fonts: SystemFonts) -> Self {
        // Tema e fontes: lidos uma única vez, ao iniciar (veja `main`).
        let scheme = sys
            .scheme
            .unwrap_or_else(|| match cc.egui_ctx.system_theme() {
                Some(egui::Theme::Light) => Scheme::Light,
                _ => Scheme::Dark,
            });
        let pal = Palette::new(scheme, sys.accent);
        pal.apply(&cc.egui_ctx);
        let font_names = (
            fonts.ui.as_ref().map(|f| f.family.clone()),
            fonts.mono.as_ref().map(|f| f.family.clone()),
        );
        crate::fonts::apply(&cc.egui_ctx, fonts);

        let ports = crate::serial::list_ports();
        Self {
            pal,
            header_compact: false,
            font_names,
            port: ports.first().cloned().unwrap_or_default(),
            ports,
            custom_port: String::new(),
            baud: 9600,
            format: 0,
            flow: 0,
            line_ending: LineEnding::CrLf,
            conn: None,
            connected_at: None,
            notice: None,
            lines: Vec::new(),
            rx_open: false,
            rx_total: 0,
            tx_total: 0,
            show_hex: false,
            timestamps: true,
            autoscroll: true,
            input: String::new(),
            input_error: None,
            send_as_hex: false,
            history: Vec::new(),
            history_pos: None,
        }
    }

    fn config(&self) -> PortConfig {
        let (_, data_bits, parity, stop_bits) = FORMATS[self.format];
        PortConfig {
            name: self.port.clone(),
            baud_rate: self.baud,
            data_bits,
            parity,
            stop_bits,
            flow_control: FLOWS[self.flow].1,
        }
    }

    fn config_label(&self) -> String {
        format!("{} · {} {}", self.port, self.baud, FORMATS[self.format].0)
    }

    // ───────────── Conexão ─────────────

    fn connect(&mut self, ctx: &egui::Context) {
        let ctx = ctx.clone();
        // Vários pedidos dentro do mesmo intervalo viram um só redesenho.
        let notify = move || ctx.request_repaint_after(FRAME_INTERVAL);
        match Connection::open(&self.config(), notify) {
            Ok(c) => {
                self.conn = Some(c);
                self.connected_at = Some(Instant::now());
                self.notice = None;
                self.push_line(Dir::Info, format!("Conectado em {}", self.config_label()));
            }
            Err(e) => {
                let msg = format!("Falha ao abrir {}: {e}", self.port);
                self.push_line(Dir::Error, msg.clone());
                self.notice = Some((msg, true));
            }
        }
    }

    fn disconnect(&mut self) {
        if self.conn.take().is_some() {
            self.push_line(Dir::Info, "Desconectado".into());
        }
        self.connected_at = None;
    }

    fn process_events(&mut self) {
        let Some(conn) = &self.conn else { return };
        let mut lost = false;
        for ev in conn.poll() {
            match ev {
                Event::Data(d) => self.push_rx(&d),
                Event::Error(e) => {
                    self.push_line(Dir::Error, e.clone());
                    self.notice = Some((e, true));
                }
                Event::Disconnected => lost = true,
            }
        }
        if lost {
            self.conn = None;
            self.connected_at = None;
            self.rx_open = false;
            self.push_line(Dir::Info, "Conexão encerrada".into());
        }
    }

    // ───────────── Terminal ─────────────

    fn push_line(&mut self, dir: Dir, text: String) {
        self.rx_open = false;
        self.lines.push(Line::new(dir, text.into_bytes()));
        self.trim();
    }

    /// Junta os bytes recebidos em linhas (quebra em '\n').
    fn push_rx(&mut self, data: &[u8]) {
        self.rx_total += data.len() as u64;
        for &b in data {
            match self.lines.last_mut() {
                Some(line) if self.rx_open => line.bytes.push(b),
                _ => {
                    self.lines.push(Line::new(Dir::Rx, vec![b]));
                    self.rx_open = true;
                }
            }
            let len = self.lines.last().map_or(0, |l| l.bytes.len());
            if b == b'\n' || len >= MAX_LINE_BYTES {
                self.rx_open = false;
            }
        }
        self.trim();
    }

    fn trim(&mut self) {
        if self.lines.len() > MAX_LINES {
            self.lines.drain(..MAX_LINES / 10);
        }
    }

    fn clear(&mut self) {
        self.lines.clear();
        self.rx_open = false;
    }

    /// O dispositivo está pedindo uma senha? (última linha recebida termina em
    /// "Password:", "Senha:" etc.) Nesse caso o que for digitado é mascarado.
    fn awaiting_secret(&self) -> bool {
        self.lines
            .last()
            .is_some_and(|l| l.dir == Dir::Rx && is_secret_prompt(&render_text(&l.bytes)))
    }

    /// Salva o terminal como texto em ~/rusted-serial-AAAAMMDD-HHMMSS.log
    fn save_log(&mut self) {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map_or_else(|| PathBuf::from("."), PathBuf::from);
        let stamp = jiff::ZonedDateTime::now().strftime("%Y%m%d-%H%M%S");

        let mut out = String::new();
        for l in &self.lines {
            let tag = match l.dir {
                Dir::Rx => "RX",
                Dir::Tx => "TX",
                Dir::Info => "--",
                Dir::Error => "!!",
            };
            let body = if l.masked {
                "[senha oculta]".into()
            } else {
                render_text(&l.bytes)
            };
            let _ = writeln!(out, "{} {tag} {body}", l.time);
        }

        self.notice = Some(
            match write_private_file(&home, &format!("rusted-serial-{stamp}"), &out) {
                Ok(path) => (format!("Log salvo em {}", path.display()), false),
                Err(e) => (format!("Não foi possível salvar o log: {e}"), true),
            },
        );
    }

    // ───────────── Envio ─────────────

    fn send_input(&mut self) {
        let Some(conn) = &self.conn else { return };
        let secret = self.awaiting_secret();
        let mut bytes = if self.send_as_hex && !secret {
            match parse_hex(&self.input) {
                Ok(b) => b,
                Err(e) => {
                    self.input_error = Some(e);
                    return;
                }
            }
        } else {
            self.input.as_bytes().to_vec()
        };
        self.input_error = None;
        bytes.extend_from_slice(self.line_ending.bytes());
        self.tx_total += bytes.len() as u64;
        conn.send(bytes.clone());

        self.rx_open = false;
        self.lines.push(Line {
            masked: secret,
            ..Line::new(Dir::Tx, bytes)
        });
        self.trim();

        // Senhas nunca entram no histórico.
        if !secret && !self.input.is_empty() && self.history.last() != Some(&self.input) {
            self.history.push(self.input.clone());
        }
        self.history_pos = None;
        self.input.clear();
    }

    fn history_step(&mut self, up: bool) {
        if self.history.is_empty() {
            return;
        }
        let last = self.history.len() - 1;
        self.history_pos = match (self.history_pos, up) {
            (None, true) => Some(last),
            (Some(0), true) => Some(0),
            (Some(i), true) => Some(i - 1),
            (Some(i), false) if i < last => Some(i + 1),
            (_, false) => None,
        };
        self.input = self
            .history_pos
            .map_or_else(String::new, |i| self.history[i].clone());
    }

    // ───────────── Partes da tela ─────────────

    /// Barra superior. Em uma linha quando cabe; em duas quando a fonte do
    /// sistema é grande ou a janela é estreita (decidido pelas larguras medidas).
    fn header(&mut self, ui: &mut egui::Ui) {
        let avail = ui.available_width();
        let compact = self.header_compact;
        let (mut brand, mut config, mut actions) = (0.0, 0.0, 0.0);
        if compact {
            ui.horizontal(|ui| {
                brand = self.header_brand(ui);
                actions = self.header_actions(ui);
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| config = self.header_config(ui));
        } else {
            ui.horizontal(|ui| {
                brand = self.header_brand(ui);
                config = self.header_config(ui);
                actions = self.header_actions(ui);
            });
        }
        let fits = brand + config + actions + 24.0 <= avail;
        if fits == compact {
            self.header_compact = !fits;
            // Refaz o quadro antes de mostrar, para não "piscar" o layout errado.
            ui.ctx().request_discard("layout da barra superior");
        }
    }

    /// Só o logo: o nome já aparece na barra de título da janela.
    fn header_brand(&self, ui: &mut egui::Ui) -> f32 {
        let w = logo(ui, &self.pal)
            .on_hover_text(concat!("Rusted Serial ", env!("CARGO_PKG_VERSION")))
            .rect
            .width();
        ui.add_space(6.0);
        w + 6.0
    }

    fn header_config(&mut self, ui: &mut egui::Ui) -> f32 {
        let p = self.pal.clone();
        let connected = self.conn.is_some();
        ui.add_enabled_ui(!connected, |ui| {
            ui.label(RichText::new("Porta").color(p.muted));
            egui::ComboBox::from_id_salt("port")
                .width(150.0)
                // não fecha ao clicar no campo "Outra porta…"
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                .selected_text(if self.port.is_empty() {
                    "—"
                } else {
                    self.port.as_str()
                })
                .show_ui(ui, |ui| {
                    if self.ports.is_empty() {
                        ui.label(RichText::new("Nenhuma porta encontrada").color(p.muted));
                    }
                    for name in &self.ports {
                        ui.selectable_value(&mut self.port, name.clone(), name);
                    }
                    // Caminho digitado à mão: /dev/pts/N (socat), /dev/rfcomm0, COM10…
                    ui.separator();
                    ui.label(RichText::new("Outra porta…").small().color(p.muted));
                    ui.horizontal(|ui| {
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut self.custom_port)
                                .hint_text("/dev/pts/3 ou COM10")
                                .desired_width(150.0),
                        );
                        let enter =
                            resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if (ui.button("Usar").clicked() || enter)
                            && !self.custom_port.trim().is_empty()
                        {
                            self.port = self.custom_port.trim().to_string();
                            ui.close();
                        }
                    });
                });
            if icons::button(
                ui,
                &p,
                Kind::Outline,
                Icon::Refresh,
                None,
                "Atualizar portas",
            )
            .on_hover_text("Atualizar portas")
            .clicked()
            {
                self.ports = crate::serial::list_ports();
                if !self.ports.contains(&self.port) {
                    self.port = self.ports.first().cloned().unwrap_or_default();
                }
            }

            ui.add_space(6.0);
            ui.label(RichText::new("Baud").color(p.muted));
            egui::ComboBox::from_id_salt("baud")
                .width(80.0)
                .selected_text(self.baud.to_string())
                .show_ui(ui, |ui| {
                    for &b in BAUD_RATES {
                        ui.selectable_value(&mut self.baud, b, b.to_string());
                    }
                });

            ui.add_space(6.0);
            ui.label(RichText::new("Formato").color(p.muted));
            egui::ComboBox::from_id_salt("format")
                .width(56.0)
                .selected_text(FORMATS[self.format].0)
                .show_ui(ui, |ui| {
                    for (i, f) in FORMATS.iter().enumerate() {
                        ui.selectable_value(&mut self.format, i, f.0);
                    }
                });

            ui.add_space(6.0);
            ui.label(RichText::new("Fluxo").color(p.muted));
            egui::ComboBox::from_id_salt("flow")
                .width(84.0)
                .selected_text(FLOWS[self.flow].0)
                .show_ui(ui, |ui| {
                    for (i, f) in FLOWS.iter().enumerate() {
                        ui.selectable_value(&mut self.flow, i, f.0);
                    }
                });
        })
        .response
        .rect
        .width()
    }

    fn header_actions(&mut self, ui: &mut egui::Ui) -> f32 {
        let p = self.pal.clone();
        let connected = self.conn.is_some();
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if connected {
                let label = "Desconectar";
                if icons::button(ui, &p, Kind::Outline, Icon::Stop, Some(label), label).clicked() {
                    self.disconnect();
                }
            } else {
                let can = !self.port.is_empty();
                let label = "Conectar";
                let clicked = ui
                    .add_enabled_ui(can, |ui| {
                        icons::button(ui, &p, Kind::Accent, Icon::Play, Some(label), label)
                    })
                    .inner
                    .clicked();
                if clicked {
                    self.connect(ui.ctx());
                }
            }
            ui.add_space(4.0);
            self.status_pill(ui);
            ui.min_rect().width()
        })
        .inner
    }

    fn status_pill(&self, ui: &mut egui::Ui) {
        let p = &self.pal;
        let body = TextStyle::Body.resolve(ui.style());
        let fmt = |font: FontId, color: Color32| TextFormat {
            font_id: font,
            color,
            ..Default::default()
        };

        let mut job = LayoutJob::default();
        let (bg, dot) = match self.connected_at {
            Some(t) => {
                let s = t.elapsed().as_secs();
                job.append("Conectado · ", 0.0, fmt(body.clone(), p.ok_text));
                // Algarismos de largura fixa: o indicador não "treme" a cada segundo.
                let mono = FontId::monospace(body.size * 0.95);
                let time = format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60);
                job.append(&time, 0.0, fmt(mono, p.ok_text));
                (p.ok_bg, p.ok_dot)
            }
            None => {
                job.append("Desconectado", 0.0, fmt(body, p.muted));
                (p.idle_bg, p.faint)
            }
        };
        // Desenhado à mão para ter exatamente o tamanho do conteúdo.
        let galley = ui.painter().layout_job(job);
        let size = egui::vec2(
            12.0 + 8.0 + 7.0 + galley.size().x + 14.0,
            (galley.size().y + 11.0).max(26.0),
        );
        let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
        let text = galley.text().to_owned();
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Status, true, &text));
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same((rect.height() / 2.0) as u8), bg);
        painter.circle_filled(egui::pos2(rect.left() + 16.0, rect.center().y), 4.0, dot);
        let text_pos = egui::pos2(rect.left() + 27.0, rect.center().y - galley.size().y / 2.0);
        painter.galley(text_pos, galley, p.text);
    }

    fn terminal(&mut self, ui: &mut egui::Ui) {
        let p = self.pal.clone();
        egui::Frame::new()
            .fill(p.terminal)
            .stroke(Stroke::new(1.0, p.border))
            .corner_radius(CornerRadius::same(10))
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());

                // Barra de ferramentas do terminal
                egui::Frame::new()
                    .fill(p.terminal_bar)
                    .corner_radius(CornerRadius {
                        nw: 10,
                        ne: 10,
                        sw: 0,
                        se: 0,
                    })
                    .inner_margin(Margin::symmetric(12, 7))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            segmented(ui, &p, &mut self.show_hex, ("Texto", false), ("HEX", true));
                            ui.add_space(8.0);
                            ui.checkbox(&mut self.timestamps, "Timestamps");
                            ui.checkbox(&mut self.autoscroll, "Auto-scroll");
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let has_lines = !self.lines.is_empty();
                                ui.add_enabled_ui(has_lines, |ui| {
                                    let l = "Limpar";
                                    if icons::button(ui, &p, Kind::Plain, Icon::Trash, Some(l), l)
                                        .clicked()
                                    {
                                        self.clear();
                                    }
                                    let l = "Salvar log";
                                    if icons::button(ui, &p, Kind::Plain, Icon::Save, Some(l), l)
                                        .clicked()
                                    {
                                        self.save_log();
                                    }
                                });
                            });
                        });
                    });
                let r = ui.available_rect_before_wrap();
                ui.painter()
                    .hline(r.x_range(), r.top(), Stroke::new(1.0, p.border));

                if self.lines.is_empty() {
                    self.empty_state(ui);
                    return;
                }

                // Linhas (só as visíveis são desenhadas: rápido mesmo com 100 mil linhas)
                let font = TextStyle::Monospace.resolve(ui.style());
                let row_h = ui.fonts_mut(|f| f.row_height(&font));
                ui.add_space(8.0);
                ui.spacing_mut().item_spacing.y = 4.0; // espaço entre linhas
                egui::ScrollArea::both()
                    .auto_shrink([false, false])
                    .stick_to_bottom(self.autoscroll)
                    .show_rows(ui, row_h, self.lines.len(), |ui, range| {
                        egui::Frame::new()
                            .inner_margin(Margin {
                                left: 12,
                                right: 12,
                                top: 0,
                                bottom: 0,
                            })
                            .show(ui, |ui| {
                                for line in &self.lines[range] {
                                    let job = self.line_job(line, &font);
                                    ui.add(
                                        egui::Label::new(job).wrap_mode(egui::TextWrapMode::Extend),
                                    );
                                }
                            });
                    });
            });
    }

    /// Terminal vazio: diz o que fazer em vez de mostrar um espaço em branco.
    fn empty_state(&self, ui: &mut egui::Ui) {
        let p = &self.pal;
        let (title, hint) = if self.conn.is_some() {
            (
                "Conectado. Aguardando dados…",
                "O que o dispositivo enviar aparece aqui.".to_string(),
            )
        } else if self.port.is_empty() {
            (
                "Nenhuma porta encontrada",
                "Conecte o cabo e clique em atualizar, ou digite o caminho em Porta → Outra porta…"
                    .to_string(),
            )
        } else {
            (
                "Nenhuma conexão",
                format!(
                    "Confira baud e formato acima e clique em Conectar para abrir {}.",
                    self.port
                ),
            )
        };
        ui.add_space((ui.available_height() / 2.0 - 30.0).max(16.0));
        ui.vertical_centered(|ui| {
            ui.label(RichText::new(title).strong().color(p.text));
            ui.add_space(4.0);
            ui.label(RichText::new(hint).color(p.muted));
        });
    }

    /// Monta uma linha colorida: horário · marcador · conteúdo.
    fn line_job(&self, line: &Line, font: &FontId) -> LayoutJob {
        let p = &self.pal;
        let fmt = |color: Color32| TextFormat {
            font_id: font.clone(),
            color,
            ..Default::default()
        };
        let mut job = LayoutJob::default();

        if self.timestamps {
            job.append(&line.time, 0.0, fmt(p.faint));
            job.append("  ", 0.0, fmt(p.faint));
        }
        let (mark, color) = match line.dir {
            Dir::Rx => (" ", p.text),
            Dir::Tx => ("›", p.accent_text),
            Dir::Info => ("•", p.muted),
            Dir::Error => ("!", p.error_text),
        };
        job.append(mark, 0.0, fmt(p.accent_text));
        job.append(" ", 0.0, fmt(color));

        let body = if line.masked {
            SECRET_MASK.to_string()
        } else if self.show_hex && matches!(line.dir, Dir::Rx | Dir::Tx) {
            render_hex(&line.bytes)
        } else {
            render_text(&line.bytes)
        };
        let body = if body.is_empty() && line.dir == Dir::Tx {
            "↵".to_string()
        } else {
            body
        };
        job.append(&body, 0.0, fmt(color));
        job
    }

    fn input_bar(&mut self, ui: &mut egui::Ui) {
        let p = self.pal.clone();
        let connected = self.conn.is_some();
        let secret = connected && self.awaiting_secret();
        let border = if self.input_error.is_some() {
            p.error_text
        } else {
            p.border
        };
        ui.horizontal(|ui| {
            let right_w = 330.0;
            let field_w = (ui.available_width() - right_w).max(200.0);

            let mut send = false;
            egui::Frame::new()
                .fill(p.field)
                .stroke(Stroke::new(1.0, border))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::symmetric(12, 2))
                .show(ui, |ui| {
                    ui.set_width(field_w - 26.0);
                    ui.horizontal(|ui| {
                        if secret {
                            let size = TextStyle::Body.resolve(ui.style()).size;
                            let (r, _) =
                                ui.allocate_exact_size(egui::Vec2::splat(size), Sense::hover());
                            icons::paint(ui.painter(), Icon::Lock, r, p.accent_text);
                        } else {
                            ui.label(RichText::new("›").monospace().strong().color(p.accent_text));
                        }
                        let hint = if secret {
                            "Senha: não aparece na tela, no histórico nem no log"
                        } else if self.send_as_hex {
                            "Bytes em HEX, ex.: AA 01 FF"
                        } else {
                            "Digite um comando e pressione Enter  (↑↓ histórico)"
                        };
                        let resp = ui.add_enabled(
                            connected,
                            egui::TextEdit::singleline(&mut self.input)
                                .hint_text(hint)
                                .password(secret)
                                .font(egui::TextStyle::Monospace)
                                .frame(egui::Frame::NONE)
                                .align(egui::Align2::LEFT_CENTER)
                                .desired_width(f32::INFINITY),
                        );
                        if resp.changed() {
                            self.input_error = None;
                        }
                        if resp.has_focus() && !secret {
                            if ui.input(|i| i.key_pressed(egui::Key::ArrowUp)) {
                                self.history_step(true);
                            } else if ui.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
                                self.history_step(false);
                            }
                        }
                        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            send = true;
                            resp.request_focus();
                        }
                    });
                });

            ui.label(RichText::new("Fim de linha").color(p.muted));
            egui::ComboBox::from_id_salt("eol")
                .width(70.0)
                .selected_text(self.line_ending.label())
                .show_ui(ui, |ui| {
                    for le in LineEnding::ALL {
                        ui.selectable_value(&mut self.line_ending, le, le.label());
                    }
                });
            if ui.checkbox(&mut self.send_as_hex, "HEX").changed() {
                self.input_error = None;
            }
            let clicked = ui
                .add_enabled_ui(connected, |ui| {
                    icons::button(ui, &p, Kind::Accent, Icon::Send, Some("Enviar"), "Enviar")
                })
                .inner
                .clicked();
            if clicked {
                send = true;
            }
            if send {
                self.send_input();
            }
        });
        // Erro junto do campo que causou, não lá no rodapé.
        if let Some(err) = &self.input_error {
            ui.add_space(4.0);
            ui.label(RichText::new(err).small().color(p.error_text));
        }
    }

    fn footer(&self, ui: &mut egui::Ui) {
        let p = &self.pal;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 18.0;
            let small = |s: String, c: Color32| RichText::new(s).small().color(c);
            if self.conn.is_some() {
                ui.label(small(self.config_label(), p.muted));
            } else {
                ui.label(small("Sem conexão".into(), p.muted));
            }
            // Contadores em monoespaçada: o que vem depois não "pula" quando mudam.
            let counter = |s: String| RichText::new(s).monospace().small().color(p.muted);
            ui.label(counter(format!("RX {:<8}", human_bytes(self.rx_total))));
            ui.label(counter(format!("TX {:<8}", human_bytes(self.tx_total))));
            if let Some((msg, is_err)) = &self.notice {
                ui.label(small(
                    msg.clone(),
                    if *is_err { p.error_text } else { p.text },
                ));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let scheme = match p.scheme {
                    Scheme::Dark => "escuro",
                    Scheme::Light => "claro",
                };
                let src = if p.accent_from_system {
                    "destaque do sistema"
                } else {
                    "destaque padrão"
                };
                let fonts = format!(
                    "Fonte da interface: {}\nFonte do terminal: {}",
                    self.font_names.0.as_deref().unwrap_or("padrão do egui"),
                    self.font_names.1.as_deref().unwrap_or("padrão do egui"),
                );
                ui.label(small(format!("Tema {scheme} · {src}"), p.muted))
                    .on_hover_text(fonts);
                let (r, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), Sense::hover());
                ui.painter().circle_filled(r.center(), 5.0, p.accent);
            });
        });
    }
}

impl eframe::App for SerialApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.process_events();
        // Atualiza o relógio de conexão uma vez por segundo.
        if self.connected_at.is_some() {
            ctx.request_repaint_after(Duration::from_secs(1));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let p = self.pal.clone();

        egui::Panel::top("header")
            .frame(
                egui::Frame::new()
                    .fill(p.header)
                    .inner_margin(Margin::symmetric(16, 10)),
            )
            .show(ui, |ui| self.header(ui));

        egui::Panel::bottom("footer")
            .frame(
                egui::Frame::new()
                    .fill(p.header)
                    .inner_margin(Margin::symmetric(16, 6)),
            )
            .show(ui, |ui| self.footer(ui));

        egui::Panel::bottom("input")
            .show_separator_line(false)
            .frame(
                egui::Frame::new()
                    .fill(p.bg)
                    .inner_margin(Margin::symmetric(16, 12)),
            )
            .show(ui, |ui| self.input_bar(ui));

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.bg).inner_margin(Margin {
                left: 16,
                right: 16,
                top: 12,
                bottom: 0,
            }))
            .show(ui, |ui| self.terminal(ui));
    }
}

// ───────────── Componentes ─────────────

/// Quadradinho com a cor de destaque e o símbolo de terminal.
fn logo(ui: &mut egui::Ui, p: &Palette) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(egui::vec2(28.0, 28.0), Sense::hover());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Image, true, "Rusted Serial"));
    let painter = ui.painter();
    painter.rect_filled(r, CornerRadius::same(7), p.accent);
    let s = Stroke::new(2.2, p.on_accent);
    let c = r.center();
    painter.line_segment([c + egui::vec2(-7.0, -5.0), c + egui::vec2(-2.0, 0.0)], s);
    painter.line_segment([c + egui::vec2(-2.0, 0.0), c + egui::vec2(-7.0, 5.0)], s);
    painter.line_segment([c + egui::vec2(1.0, 6.0), c + egui::vec2(8.0, 6.0)], s);
    resp
}

/// Botões segmentados (ex.: Texto | HEX). Leitores de tela anunciam qual está ativo.
fn segmented<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    p: &Palette,
    value: &mut T,
    a: (&str, T),
    b: (&str, T),
) {
    egui::Frame::new()
        .fill(p.field)
        .corner_radius(CornerRadius::same(7))
        .inner_margin(Margin::same(2))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for (label, v) in [a, b] {
                    let on = *value == v;
                    let btn = egui::Button::new(RichText::new(label).strong().color(if on {
                        p.on_accent
                    } else {
                        p.muted
                    }))
                    .selected(on)
                    .fill(if on { p.accent } else { Color32::TRANSPARENT })
                    .stroke(Stroke::NONE)
                    .corner_radius(CornerRadius::same(5));
                    if ui.add(btn).clicked() {
                        *value = v;
                    }
                }
            });
        });
}

// ───────────── Arquivos ─────────────

/// Cria `<dir>/<stem>.log` legível só pelo usuário (0600 no Linux) sem
/// sobrescrever nada; se o nome já existir, tenta `<stem>-2.log`, `-3`…
fn write_private_file(dir: &Path, stem: &str, content: &str) -> std::io::Result<PathBuf> {
    let mut opts = std::fs::OpenOptions::new();
    // create_new: falha se já existir (inclusive um link simbólico com esse nome).
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);

    for n in 1..=99 {
        let name = if n == 1 {
            format!("{stem}.log")
        } else {
            format!("{stem}-{n}.log")
        };
        let path = dir.join(name);
        match opts.open(&path) {
            Ok(mut file) => {
                file.write_all(content.as_bytes())?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "nomes de arquivo esgotados",
    ))
}

// ───────────── Formatação ─────────────

/// Bytes → texto exibível: ignora \r e \n e troca por '·' os caracteres de
/// controle e os invisíveis de direção/formatação (U+202E etc.), que um
/// dispositivo poderia usar para embaralhar visualmente o terminal e o log.
fn render_text(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len());
    for ch in String::from_utf8_lossy(bytes).chars() {
        match ch {
            '\r' | '\n' => {}
            '\t' => s.push_str("    "),
            c if c.is_control() || is_invisible_format(c) => s.push('·'),
            c => s.push(c),
        }
    }
    s
}

/// Caracteres invisíveis de formatação/direção (bidi, largura zero, BOM).
fn is_invisible_format(c: char) -> bool {
    matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}')
}

fn render_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        let _ = write!(s, "{b:02X}"); // escrever numa String nunca falha
    }
    s
}

fn human_bytes(n: u64) -> String {
    let f = n as f64; // perda de precisão irrelevante: só exibe 1 casa decimal
    match n {
        0..1024 => format!("{n} B"),
        1024..1_048_576 => format!("{:.1} KB", f / 1024.0).replace('.', ","),
        _ => format!("{:.1} MB", f / 1_048_576.0).replace('.', ","),
    }
}

/// O texto parece um pedido de senha? ("Password:", "Enter PIN:", "Senha: ")
fn is_secret_prompt(text: &str) -> bool {
    let t = text.trim_end().to_lowercase();
    t.ends_with(':')
        && ["password", "passphrase", "senha", "pin", "secret"]
            .iter()
            .any(|w| t.contains(w))
}

/// Converte "AA 01 ff" ou "AA01FF" em bytes.
fn parse_hex(s: &str) -> Result<Vec<u8>, String> {
    let digits: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    // Valida antes de converter: só dígitos HEX ASCII (evita cortar um "é" ao meio).
    if let Some(bad) = s
        .chars()
        .find(|c| !c.is_whitespace() && !c.is_ascii_hexdigit())
    {
        return Err(format!(
            "HEX inválido: '{bad}' não é um dígito hexadecimal (0-9, A-F)"
        ));
    }
    if !digits.len().is_multiple_of(2) {
        return Err("HEX inválido: número ímpar de dígitos (cada byte usa 2, ex.: 0A)".into());
    }
    let nibble = |d: u8| char::from(d).to_digit(16).unwrap_or(0) as u8;
    // Já validado acima: número par de dígitos, então não sobra resto.
    let (pairs, _) = digits.as_chunks::<2>();
    Ok(pairs
        .iter()
        .map(|&[hi, lo]| nibble(hi) << 4 | nibble(lo))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    mod parse_hex {
        use super::*;

        #[test]
        fn aceita_bytes_separados_por_espaco_e_minusculas() {
            assert_eq!(parse_hex("AA 01 ff"), Ok(vec![0xAA, 0x01, 0xFF]));
        }

        #[test]
        fn aceita_bytes_colados() {
            assert_eq!(parse_hex("0a0D"), Ok(vec![0x0A, 0x0D]));
        }

        #[test]
        fn recusa_numero_impar_de_digitos() {
            assert!(parse_hex("ABC").is_err());
        }

        #[test]
        fn recusa_letras_fora_do_hexadecimal() {
            assert!(parse_hex("ZZ").is_err());
        }

        #[test]
        fn recusa_acentos_sem_entrar_em_panic() {
            // Antes, "aéa" cortava o "é" ao meio e derrubava o app.
            assert!(parse_hex("aéa").is_err());
        }
    }

    mod render_text {
        use super::*;

        #[test]
        fn remove_quebras_de_linha() {
            assert_eq!(render_text(b"Switch#\r\n"), "Switch#");
        }

        #[test]
        fn troca_caracteres_de_controle_por_ponto() {
            assert_eq!(render_text(&[0x41, 0x01, 0x42]), "A·B");
        }

        #[test]
        fn troca_inversao_de_direcao_unicode_por_ponto() {
            assert_eq!(render_text("ok\u{202E}txe".as_bytes()), "ok·txe");
        }
    }

    #[test]
    fn render_hex_separa_bytes_com_espaco() {
        assert_eq!(render_hex(&[0xAA, 0x0D, 0x0A]), "AA 0D 0A");
    }

    #[test]
    fn human_bytes_mostra_bytes_abaixo_de_1_kb() {
        assert_eq!(human_bytes(312), "312 B");
    }

    #[test]
    fn human_bytes_usa_virgula_decimal_em_kb() {
        assert_eq!(human_bytes(12_697), "12,4 KB");
    }

    mod is_secret_prompt {
        use super::*;

        #[test]
        fn reconhece_password_do_cisco() {
            assert!(is_secret_prompt("Password: "));
        }

        #[test]
        fn reconhece_senha_em_portugues() {
            assert!(is_secret_prompt("Senha:"));
        }

        #[test]
        fn ignora_texto_que_so_menciona_senha() {
            assert!(!is_secret_prompt("password changed successfully"));
        }

        #[test]
        fn ignora_prompt_comum() {
            assert!(!is_secret_prompt("Switch#"));
        }
    }

    mod write_private_file {
        use super::*;

        #[test]
        fn nao_sobrescreve_arquivo_existente() {
            let dir = std::env::temp_dir().join(format!("rs-log-test-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let first = write_private_file(&dir, "log", "a").unwrap();
            let second = write_private_file(&dir, "log", "b").unwrap();
            assert_ne!(first, second);
            std::fs::remove_dir_all(&dir).unwrap();
        }

        #[cfg(unix)]
        #[test]
        fn cria_arquivo_legivel_so_pelo_usuario() {
            use std::os::unix::fs::PermissionsExt;
            let dir = std::env::temp_dir().join(format!("rs-perm-test-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = write_private_file(&dir, "log", "x").unwrap();
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            std::fs::remove_dir_all(&dir).unwrap();
            assert_eq!(mode, 0o600);
        }
    }
}
