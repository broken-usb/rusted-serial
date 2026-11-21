//! Protocolo Telnet (RFC 854) no mínimo necessário para conversar com
//! switches e roteadores: separa os comandos `IAC` dos dados, responde às
//! negociações e escapa o byte 0xFF no envio.
//!
//! Aceitamos só duas opções: o servidor ecoar o que digitamos (ECHO) e o modo
//! sem "go-ahead" (SGA). As demais são recusadas, o que todo servidor aceita.

const IAC: u8 = 255;
const DONT: u8 = 254;
const DO: u8 = 253;
const WONT: u8 = 252;
const WILL: u8 = 251;
const SB: u8 = 250;
const SE: u8 = 240;

const ECHO: u8 = 1;
const SGA: u8 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Data,
    /// Depois de um CR: um NUL em seguida é só preenchimento (CR NUL = CR).
    Cr,
    Iac,
    /// IAC + DO/DONT/WILL/WONT: falta o byte da opção.
    Negotiate(u8),
    /// Subnegociação (IAC SB ... IAC SE): ignorada.
    Sub,
    SubIac,
}

pub struct Telnet {
    state: State,
    /// Opções que nós ativamos (respondemos WILL).
    ours: [bool; 256],
    /// Opções que o servidor ativou (respondemos DO).
    theirs: [bool; 256],
}

impl Telnet {
    pub fn new() -> Self {
        Self {
            state: State::Data,
            ours: [false; 256],
            theirs: [false; 256],
        }
    }

    /// Separa os dados das negociações. Retorna (dados, resposta a enviar).
    pub fn decode(&mut self, input: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let mut data = Vec::with_capacity(input.len());
        let mut reply = Vec::new();
        for &b in input {
            self.state = match (self.state, b) {
                (State::Cr, 0) => State::Data,
                (State::Data | State::Cr, IAC) => State::Iac,
                (State::Data | State::Cr, b'\r') => {
                    data.push(b);
                    State::Cr
                }
                (State::Data | State::Cr, _) => {
                    data.push(b);
                    State::Data
                }
                (State::Iac, IAC) => {
                    data.push(IAC); // IAC IAC = o byte 0xFF
                    State::Data
                }
                (State::Iac, DO | DONT | WILL | WONT) => State::Negotiate(b),
                (State::Iac, SB) => State::Sub,
                (State::Iac, _) => State::Data, // NOP, GA etc.
                (State::Negotiate(cmd), opt) => {
                    self.negotiate(cmd, opt, &mut reply);
                    State::Data
                }
                (State::Sub, IAC) => State::SubIac,
                (State::Sub, _) => State::Sub,
                (State::SubIac, SE) => State::Data,
                (State::SubIac, _) => State::Sub,
            };
        }
        (data, reply)
    }

    /// Responde só quando o estado muda: evita laços de negociação.
    fn negotiate(&mut self, cmd: u8, opt: u8, reply: &mut Vec<u8>) {
        let i = usize::from(opt);
        match cmd {
            DO if opt == SGA => {
                if !self.ours[i] {
                    self.ours[i] = true;
                    reply.extend([IAC, WILL, opt]);
                }
            }
            DO => reply.extend([IAC, WONT, opt]),
            DONT if self.ours[i] => {
                self.ours[i] = false;
                reply.extend([IAC, WONT, opt]);
            }
            WILL if matches!(opt, ECHO | SGA) => {
                if !self.theirs[i] {
                    self.theirs[i] = true;
                    reply.extend([IAC, DO, opt]);
                }
            }
            WILL => reply.extend([IAC, DONT, opt]),
            WONT if self.theirs[i] => {
                self.theirs[i] = false;
                reply.extend([IAC, DONT, opt]);
            }
            _ => {}
        }
    }
}

/// Prepara bytes para envio: 0xFF vira IAC IAC e um CR solto vira CR NUL
/// (no Telnet, CR sempre vem seguido de LF ou NUL).
pub fn encode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 2);
    for (i, &b) in data.iter().enumerate() {
        out.push(b);
        match b {
            IAC => out.push(IAC),
            b'\r' if data.get(i + 1) != Some(&b'\n') => out.push(0),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separa_dados_das_negociacoes() {
        let mut t = Telnet::new();
        let (data, reply) = t.decode(&[IAC, WILL, ECHO, b'o', b'k', IAC, DO, 24]);
        assert_eq!(data, b"ok");
        // aceita o eco do servidor, recusa TTYPE (24)
        assert_eq!(reply, [IAC, DO, ECHO, IAC, WONT, 24]);
    }

    #[test]
    fn nao_responde_de_novo_ao_que_ja_aceitou() {
        let mut t = Telnet::new();
        t.decode(&[IAC, WILL, ECHO]);
        let (_, reply) = t.decode(&[IAC, WILL, ECHO]);
        assert!(reply.is_empty());
    }

    #[test]
    fn negociacao_pode_chegar_partida() {
        let mut t = Telnet::new();
        let (d1, r1) = t.decode(&[b'a', IAC]);
        let (d2, r2) = t.decode(&[WILL, SGA, b'b']);
        assert_eq!((d1, d2), (b"a".to_vec(), b"b".to_vec()));
        assert!(r1.is_empty());
        assert_eq!(r2, [IAC, DO, SGA]);
    }

    #[test]
    fn ignora_subnegociacao_e_le_iac_iac_como_0xff() {
        let mut t = Telnet::new();
        let (data, _) = t.decode(&[IAC, SB, 24, 1, IAC, SE, IAC, IAC, b'x']);
        assert_eq!(data, [0xFF, b'x']);
    }

    #[test]
    fn cr_nul_vira_cr() {
        let mut t = Telnet::new();
        let (data, _) = t.decode(b"a\r\0b\r\n");
        assert_eq!(data, b"a\rb\r\n");
    }

    #[test]
    fn encode_escapa_iac_e_completa_cr() {
        assert_eq!(encode(&[0xFF]), [0xFF, 0xFF]);
        assert_eq!(encode(b"ls\r"), b"ls\r\0");
        assert_eq!(encode(b"ls\r\n"), b"ls\r\n");
    }
}
