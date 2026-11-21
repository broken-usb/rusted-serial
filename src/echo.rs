//! Esconde o eco do que você digitou.
//!
//! Em SSH, ADB e Telnet o outro lado devolve cada comando enviado (o terminal
//! remoto "ecoa"). Como o app já mostra o que você enviou numa linha
//! destacada, o eco apareceria duplicado. Este filtro segura os bytes
//! recebidos enquanto eles batem com o que foi enviado: se bater até o fim, o
//! eco (e a quebra de linha logo depois) some; se divergir, tudo o que foi
//! segurado é liberado sem perder nada.
//!
//! Sequências `ESC [ ... ` no meio do eco (cores de realce do zsh, modo
//! "bracketed paste" do bash) são ignoradas na comparação.

use std::time::{Duration, Instant};

/// Desiste de esperar o eco depois disso e libera o que estava segurado.
const PATIENCE: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Phase {
    #[default]
    Idle,
    /// Comparando os bytes recebidos com o que foi enviado.
    Matching,
    /// O eco bateu: falta engolir o "\r\n" que vem em seguida.
    Tail,
}

#[derive(Default)]
pub struct EchoFilter {
    phase: Phase,
    expect: Vec<u8>,
    pos: usize,
    held: Vec<u8>,
    esc: bool,
    csi: bool,
    since: Option<Instant>,
}

impl EchoFilter {
    /// Começa a esperar o eco de `sent` (o fim de linha é ignorado). Devolve o
    /// que estava segurado de uma espera anterior, para ser mostrado.
    pub fn arm(&mut self, sent: &[u8]) -> Vec<u8> {
        let released = self.release();
        let end = sent
            .iter()
            .rposition(|&b| b != b'\r' && b != b'\n')
            .map_or(0, |i| i + 1);
        self.expect = sent[..end].to_vec();
        self.pos = 0;
        // Linha vazia: o eco é só a quebra de linha.
        self.phase = if self.expect.is_empty() {
            Phase::Tail
        } else {
            Phase::Matching
        };
        self.since = Some(Instant::now());
        released
    }

    /// Ainda esperando o eco?
    pub fn active(&self) -> bool {
        self.phase != Phase::Idle
    }

    /// Filtra os bytes recebidos; devolve o que deve aparecer.
    pub fn filter(&mut self, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for (i, &b) in data.iter().enumerate() {
            match self.phase {
                Phase::Idle => {
                    out.extend_from_slice(&data[i..]);
                    return out;
                }
                Phase::Matching => {
                    if self.skip_escape(b) {
                        self.held.push(b);
                    } else if self.expect.get(self.pos) == Some(&b) {
                        self.held.push(b);
                        self.pos += 1;
                        if self.pos == self.expect.len() {
                            self.held.clear();
                            self.phase = Phase::Tail;
                        }
                    } else {
                        out.append(&mut self.held);
                        self.stop();
                        out.extend_from_slice(&data[i..]);
                        return out;
                    }
                }
                Phase::Tail => {
                    if self.skip_escape(b) || b == b'\r' {
                        continue;
                    }
                    self.stop();
                    if b != b'\n' {
                        out.extend_from_slice(&data[i..]);
                        return out;
                    }
                }
            }
        }
        out
    }

    /// Se o eco não veio a tempo, libera o que estava segurado.
    pub fn stale(&mut self, now: Instant) -> Option<Vec<u8>> {
        let late = self.since.is_some_and(|t| now.duration_since(t) > PATIENCE);
        (self.active() && late).then(|| self.release())
    }

    fn release(&mut self) -> Vec<u8> {
        self.stop();
        std::mem::take(&mut self.held)
    }

    fn stop(&mut self) {
        self.phase = Phase::Idle;
        self.esc = false;
        self.csi = false;
        self.since = None;
    }

    /// Está dentro de uma sequência `ESC [ ...` (ou `ESC x`)?
    fn skip_escape(&mut self, b: u8) -> bool {
        if self.csi {
            if (0x40..=0x7e).contains(&b) {
                self.csi = false;
            }
            return true;
        }
        if self.esc {
            self.esc = false;
            self.csi = b == b'[';
            return true;
        }
        if b == 0x1b {
            self.esc = true;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn armed(sent: &[u8]) -> EchoFilter {
        let mut f = EchoFilter::default();
        f.arm(sent);
        f
    }

    #[test]
    fn some_com_o_eco_e_a_quebra_de_linha() {
        let mut f = armed(b"ls -la\r");
        assert_eq!(f.filter(b"ls -la\r\ntotal 8\r\n"), b"total 8\r\n");
        assert!(!f.active());
    }

    #[test]
    fn eco_pode_chegar_aos_pedacos() {
        let mut f = armed(b"show vlan\r\n");
        assert!(f.filter(b"show").is_empty());
        assert!(f.filter(b" vlan\r").is_empty());
        assert_eq!(f.filter(b"\nVLAN Name"), b"VLAN Name");
    }

    #[test]
    fn sem_eco_nada_se_perde() {
        let mut f = armed(b"ls\r");
        assert_eq!(f.filter(b"lx\r\n"), b"lx\r\n");
        assert_eq!(f.filter(b"mais"), b"mais");
    }

    #[test]
    fn ignora_cores_no_meio_do_eco_e_bracketed_paste() {
        let mut f = armed(b"ls\r");
        let data = b"\x1b[32ml\x1b[0ms\x1b[?2004l\r\r\nREADME\r\n";
        assert_eq!(f.filter(data), b"README\r\n");
    }

    #[test]
    fn linha_vazia_engole_so_a_quebra() {
        let mut f = armed(b"\r");
        assert_eq!(f.filter(b"\r\nSwitch#"), b"Switch#");
    }

    #[test]
    fn libera_o_segurado_se_o_eco_nao_terminar() {
        let mut f = armed(b"reboot\r");
        assert!(f.filter(b"reb").is_empty());
        assert_eq!(
            f.stale(Instant::now() + PATIENCE * 2),
            Some(b"reb".to_vec())
        );
        assert!(!f.active());
    }

    #[test]
    fn novo_envio_libera_a_espera_anterior() {
        let mut f = armed(b"abc\r");
        f.filter(b"ab");
        assert_eq!(f.arm(b"xyz\r"), b"ab");
    }
}
