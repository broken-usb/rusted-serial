//! Códigos ANSI no terminal em modo linha.
//!
//! Shells (SSH, ADB, Telnet) e muitos firmwares (logs do ESP32, por exemplo)
//! mandam cores como `ESC [ 32 m`. Aqui interpretamos só as cores e estilos
//! (SGR) e descartamos o resto: posicionar cursor, título da janela (OSC),
//! modos do terminal. Assim o texto chega limpo e colorido, sem lixo como
//! `[0;32m` no meio.
//!
//! Cada linha guarda o estilo em que começou: uma cor ligada numa linha
//! continua valendo nas seguintes, como num terminal de verdade.

/// Cor ANSI: uma das 256 da paleta ou RGB direto.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Color {
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
}

/// Trecho de texto visível com um mesmo estilo.
#[derive(Debug, PartialEq, Eq)]
pub struct Span {
    pub style: Style,
    pub text: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Text,
    Esc,
    Csi,
    /// OSC (título da janela etc.): termina em BEL ou ESC \.
    Osc,
    /// DCS, SOS, PM, APC: terminam em ESC \.
    Str,
    StrEsc,
    /// ESC ( B e parecidos: falta um caractere.
    Charset,
}

/// Interpreta uma linha a partir do estilo `start`. Devolve os trechos
/// visíveis e o estilo em que a linha termina.
pub fn parse(start: Style, bytes: &[u8]) -> (Vec<Span>, Style) {
    let mut spans: Vec<Span> = Vec::new();
    let mut style = start;
    let mut text = String::new();
    let mut params = String::new();
    let mut state = State::Text;

    let flush = |spans: &mut Vec<Span>, text: &mut String, style: Style| {
        if !text.is_empty() {
            spans.push(Span {
                style,
                text: std::mem::take(text),
            });
        }
    };

    for ch in String::from_utf8_lossy(bytes).chars() {
        state = match (state, ch) {
            (State::Text, '\x1b') => State::Esc,
            (State::Text, '\u{9b}') => {
                params.clear();
                State::Csi
            }
            (State::Text, c) => {
                match c {
                    '\r' | '\n' | '\x07' | '\x7f' => {}
                    '\t' => text.push_str("    "),
                    // Backspace apaga o caractere anterior (eco de edição de linha).
                    '\x08' => {
                        text.pop();
                    }
                    c if c.is_control() || is_invisible_format(c) => text.push('·'),
                    c => text.push(c),
                }
                State::Text
            }
            (State::Esc, '[') => {
                params.clear();
                State::Csi
            }
            (State::Esc, ']') => State::Osc,
            (State::Esc, 'P' | 'X' | '^' | '_') => State::Str,
            (State::Esc, '(' | ')' | '*' | '+' | '-' | '.' | '/' | '#' | '%') => State::Charset,
            (State::Esc, _) => State::Text, // ESC 7, ESC =, ESC M...
            (State::Csi, c @ '\x40'..='\x7e') => {
                if c == 'm' && !params.starts_with(['?', '<', '=', '>']) {
                    let before = style;
                    apply_sgr(&params, &mut style);
                    if style != before {
                        flush(&mut spans, &mut text, before);
                    }
                }
                State::Text
            }
            (State::Csi, '\x1b') => State::Esc,
            (State::Csi, c) if params.len() < 64 && !c.is_control() => {
                params.push(c);
                State::Csi
            }
            (State::Csi, _) => State::Text, // sequência inválida
            (State::Osc, '\x07') => State::Text,
            (State::Osc | State::Str, '\x1b') => State::StrEsc,
            (State::Osc, _) => State::Osc,
            (State::Str, _) => State::Str,
            (State::StrEsc, '\\') => State::Text,
            (State::StrEsc, _) => State::Text,
            (State::Charset, _) => State::Text,
        };
    }
    flush(&mut spans, &mut text, style);
    (spans, style)
}

/// Só o texto visível (para o log e para reconhecer pedidos de senha).
pub fn plain(bytes: &[u8]) -> String {
    parse(Style::default(), bytes)
        .0
        .into_iter()
        .map(|s| s.text)
        .collect()
}

/// Aplica os parâmetros de `ESC [ ... m`.
fn apply_sgr(params: &str, style: &mut Style) {
    if params.is_empty() {
        *style = Style::default();
        return;
    }
    let num = |s: &str| s.parse::<u16>().unwrap_or(0);
    let mut groups = params.split(';');
    while let Some(group) = groups.next() {
        // Forma com dois-pontos: "38:2::255:0:0" ou "38:5:208".
        if group.contains(':') {
            let sub: Vec<u16> = group.split(':').map(num).collect();
            let color = match sub.as_slice() {
                [_, 5, n, ..] => Some(Color::Indexed(byte(*n))),
                [_, 2, _, r, g, b] | [_, 2, r, g, b] => {
                    Some(Color::Rgb(byte(*r), byte(*g), byte(*b)))
                }
                _ => None,
            };
            match sub.first() {
                Some(38) => style.fg = color,
                Some(48) => style.bg = color,
                _ => {}
            }
            continue;
        }
        match num(group) {
            0 => *style = Style::default(),
            1 => style.bold = true,
            2 => style.dim = true,
            3 => style.italic = true,
            4 => style.underline = true,
            7 => style.inverse = true,
            22 => (style.bold, style.dim) = (false, false),
            23 => style.italic = false,
            24 => style.underline = false,
            27 => style.inverse = false,
            n @ 30..=37 => style.fg = Some(Color::Indexed(byte(n - 30))),
            n @ 90..=97 => style.fg = Some(Color::Indexed(byte(n - 90 + 8))),
            n @ 40..=47 => style.bg = Some(Color::Indexed(byte(n - 40))),
            n @ 100..=107 => style.bg = Some(Color::Indexed(byte(n - 100 + 8))),
            39 => style.fg = None,
            49 => style.bg = None,
            n @ (38 | 48) => {
                let color = match groups.next().map(num) {
                    Some(5) => groups.next().map(|i| Color::Indexed(byte(num(i)))),
                    Some(2) => {
                        let mut c = || groups.next().map_or(0, |v| byte(num(v)));
                        let (r, g, b) = (c(), c(), c());
                        Some(Color::Rgb(r, g, b))
                    }
                    _ => None,
                };
                if n == 38 {
                    style.fg = color;
                } else {
                    style.bg = color;
                }
            }
            _ => {}
        }
    }
}

fn byte(n: u16) -> u8 {
    u8::try_from(n).unwrap_or(u8::MAX)
}

/// Caracteres invisíveis de formatação/direção (bidi, largura zero, BOM): um
/// dispositivo poderia usá-los para embaralhar visualmente o terminal e o log.
fn is_invisible_format(c: char) -> bool {
    matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}')
}

/// RGB de uma cor da paleta de 256 (0–15 são as básicas; quem desenha pode
/// trocar essas por tons do tema).
pub fn xterm_rgb(i: u8) -> (u8, u8, u8) {
    const BASIC: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (205, 49, 49),
        (13, 188, 121),
        (229, 229, 16),
        (36, 114, 200),
        (188, 63, 188),
        (17, 168, 205),
        (229, 229, 229),
        (102, 102, 102),
        (241, 76, 76),
        (35, 209, 139),
        (245, 245, 67),
        (59, 142, 234),
        (214, 112, 214),
        (41, 184, 219),
        (255, 255, 255),
    ];
    match i {
        0..=15 => BASIC[usize::from(i)],
        16..=231 => {
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            let n = i - 16;
            (level(n / 36), level((n / 6) % 6), level(n % 6))
        }
        _ => {
            let v = 8 + (i - 232) * 10;
            (v, v, v)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fg(i: u8) -> Option<Color> {
        Some(Color::Indexed(i))
    }

    #[test]
    fn texto_sem_codigos_passa_igual() {
        assert_eq!(plain(b"Switch#\r\n"), "Switch#");
    }

    #[test]
    fn troca_caracteres_de_controle_por_ponto() {
        assert_eq!(plain(&[0x41, 0x01, 0x42]), "A·B");
    }

    #[test]
    fn troca_inversao_de_direcao_unicode_por_ponto() {
        assert_eq!(plain("ok\u{202E}txe".as_bytes()), "ok·txe");
    }

    #[test]
    fn separa_trechos_por_cor() {
        let (spans, end) = parse(Style::default(), b"\x1b[32mOK\x1b[0m fim");
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].text, "OK");
        assert_eq!(spans[0].style.fg, fg(2));
        assert_eq!(spans[1].text, " fim");
        assert_eq!(end, Style::default());
    }

    #[test]
    fn cor_continua_na_linha_seguinte() {
        let (_, end) = parse(Style::default(), b"\x1b[1;31mErro: ");
        assert_eq!(end.fg, fg(1));
        assert!(end.bold);
        let (spans, _) = parse(end, b"continua vermelho");
        assert_eq!(spans[0].style.fg, fg(1));
    }

    #[test]
    fn cores_256_e_rgb() {
        let (spans, _) = parse(
            Style::default(),
            b"\x1b[38;5;208ma\x1b[38;2;1;2;3mb\x1b[48:2::9:8:7mc",
        );
        assert_eq!(spans[0].style.fg, fg(208));
        assert_eq!(spans[1].style.fg, Some(Color::Rgb(1, 2, 3)));
        assert_eq!(spans[2].style.bg, Some(Color::Rgb(9, 8, 7)));
    }

    #[test]
    fn descarta_titulo_e_outros_codigos() {
        // Título da janela (OSC), modo bracketed paste, limpar linha, charset.
        let line = b"\x1b]0;joao@pi: ~\x07\x1b[?2004hjoao@pi:~$ \x1b[K\x1b(B";
        assert_eq!(plain(line), "joao@pi:~$ ");
    }

    #[test]
    fn backspace_apaga_o_anterior() {
        assert_eq!(plain(b"lss\x08 \x08"), "ls");
    }

    #[test]
    fn esp_idf_log() {
        let (spans, end) = parse(
            Style::default(),
            b"\x1b[0;32mI (312) wifi: conectado\x1b[0m\r\n",
        );
        assert_eq!(spans[0].text, "I (312) wifi: conectado");
        assert_eq!(spans[0].style.fg, fg(2));
        assert_eq!(end, Style::default());
    }

    #[test]
    fn paleta_256() {
        assert_eq!(xterm_rgb(16), (0, 0, 0));
        assert_eq!(xterm_rgb(196), (255, 0, 0));
        assert_eq!(xterm_rgb(255), (238, 238, 238));
    }
}
