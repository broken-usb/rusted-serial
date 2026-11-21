//! Shell de aparelhos Android pelo servidor do adb (o mesmo que o comando
//! `adb` usa, em 127.0.0.1:5037). Assim valem a autorização de depuração já
//! feita no aparelho, o USB e o "adb connect" por Wi-Fi, sem driver próprio.
//!
//! Protocolo: cada pedido é `<tamanho em 4 dígitos hex><texto>`; a resposta é
//! `OKAY` ou `FAIL<tamanho><mensagem>`. Depois de `shell:` a conexão vira um
//! fluxo de bytes com o terminal do aparelho.

use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::process::{Command as Process, Stdio};
use std::time::Duration;

use super::{Commands, Event, Exit, Link, block_on, net};

const SERVER: SocketAddr = SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::LOCALHOST), 5037);

/// Um aparelho visto pelo servidor do adb.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub serial: String,
    /// "device" quando pronto; também "unauthorized", "offline"...
    pub state: String,
    pub model: Option<String>,
}

impl Device {
    pub fn label(&self) -> String {
        let name = match &self.model {
            Some(m) => format!("{m} ({})", self.serial),
            None => self.serial.clone(),
        };
        match self.state.as_str() {
            "device" => name,
            "unauthorized" => format!("{name} · não autorizado"),
            other => format!("{name} · {other}"),
        }
    }
}

/// Lista os aparelhos. Rápido: só conversa com o servidor local, e não tenta
/// iniciá-lo (isso fica para a hora de conectar, fora da interface).
///
/// # Errors
/// Mensagem amigável se o servidor do adb não estiver rodando.
pub fn list_devices() -> Result<Vec<Device>, String> {
    let mut s = TcpStream::connect_timeout(&SERVER, Duration::from_millis(300))
        .map_err(|_| "Servidor do adb parado: ele é iniciado ao conectar".to_string())?;
    let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
    request(&mut s, "host:devices-l").map_err(|e| e.to_string())?;
    status(&mut s)?;
    Ok(parse_devices(
        &read_block(&mut s).map_err(|e| e.to_string())?,
    ))
}

pub(super) fn run(device: Option<&str>, cmds: &mut Commands, link: &Link) -> Exit {
    // O pedido ao servidor local é rápido e pode ser bloqueante; daí em
    // diante a conexão vira um fluxo de bytes como o TCP.
    let stream = match open_shell(device).and_then(|s| {
        s.set_nonblocking(true)
            .map(|()| s)
            .map_err(|e| format!("Erro ao configurar a conexão: {e}"))
    }) {
        Ok(s) => s,
        Err(msg) => return Exit::Failed(msg),
    };
    block_on(async {
        let stream = match tokio::net::TcpStream::from_std(stream) {
            Ok(s) => s,
            Err(e) => return Exit::Failed(format!("Erro ao configurar a conexão: {e}")),
        };
        if !link.emit(Event::Connected(String::new())) {
            return Exit::UiGone;
        }
        net::pump(stream, cmds, link, None).await
    })
}

fn open_shell(device: Option<&str>) -> Result<TcpStream, String> {
    let io_err = |e: io::Error| format!("Erro ao falar com o servidor do adb: {e}");
    // "192.168.0.10:5555": aparelho por Wi-Fi. `adb connect` é idempotente.
    if let Some(addr) = device.filter(|d| d.contains(':')) {
        let mut s = server()?;
        request(&mut s, &format!("host:connect:{addr}")).map_err(io_err)?;
        status(&mut s)?;
        let msg = read_block(&mut s).map_err(io_err)?;
        if !msg.contains("connected to") {
            return Err(format!("adb connect {addr}: {}", msg.trim()));
        }
    }

    let mut s = server()?;
    let transport = device.map_or_else(
        || "host:transport-any".to_string(),
        |d| format!("host:transport:{d}"),
    );
    request(&mut s, &transport).map_err(io_err)?;
    status(&mut s).map_err(|e| friendly(&e))?;
    // Sem comando: shell interativo com terminal (prompt, eco, Ctrl+C).
    request(&mut s, "shell:").map_err(io_err)?;
    status(&mut s).map_err(|e| friendly(&e))?;
    Ok(s)
}

/// Conecta no servidor do adb; se estiver parado, inicia com `adb start-server`.
fn server() -> Result<TcpStream, String> {
    let connect = || TcpStream::connect_timeout(&SERVER, Duration::from_secs(2));
    let stream = match connect() {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {
            start_server()?;
            connect().map_err(|e| format!("O servidor do adb não respondeu: {e}"))?
        }
        Err(e) => return Err(format!("O servidor do adb não respondeu: {e}")),
    };
    // Só para os pedidos iniciais (depois a conexão fica não bloqueante).
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    Ok(stream)
}

fn start_server() -> Result<(), String> {
    let mut cmd = Process::new("adb");
    cmd.arg("start-server")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        // Sem piscar uma janela de console (o app é uma janela gráfica).
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    match cmd.status() {
        Ok(st) if st.success() => Ok(()),
        Ok(st) => Err(format!("`adb start-server` falhou ({st})")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Err(
            "Não encontrei o adb. Instale o Android SDK Platform-Tools e deixe o adb no PATH."
                .into(),
        ),
        Err(e) => Err(format!("Não consegui rodar o adb: {e}")),
    }
}

fn request(s: &mut TcpStream, payload: &str) -> io::Result<()> {
    s.write_all(format!("{:04x}{payload}", payload.len()).as_bytes())
}

/// Lê `OKAY` ou `FAIL` + mensagem.
fn status(s: &mut TcpStream) -> Result<(), String> {
    let mut word = [0u8; 4];
    s.read_exact(&mut word)
        .map_err(|e| format!("O servidor do adb não respondeu: {e}"))?;
    match &word {
        b"OKAY" => Ok(()),
        b"FAIL" => Err(read_block(s).unwrap_or_default()),
        _ => Err("Resposta inesperada do servidor do adb".into()),
    }
}

/// Bloco com tamanho em 4 dígitos hex na frente.
fn read_block(s: &mut TcpStream) -> io::Result<String> {
    let mut len = [0u8; 4];
    s.read_exact(&mut len)?;
    let len = std::str::from_utf8(&len)
        .ok()
        .and_then(|h| usize::from_str_radix(h, 16).ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "tamanho inválido"))?;
    let mut body = vec![0u8; len];
    s.read_exact(&mut body)?;
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// Saída de `host:devices-l`: "SERIAL  device product:x model:Pixel_7 ...".
fn parse_devices(text: &str) -> Vec<Device> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let serial = parts.next()?.to_string();
            let state = parts.next()?.to_string();
            let model = parts
                .find_map(|p| p.strip_prefix("model:"))
                .map(|m| m.replace('_', " "));
            Some(Device {
                serial,
                state,
                model,
            })
        })
        .collect()
}

/// Traduz as recusas mais comuns do adb.
fn friendly(msg: &str) -> String {
    let m = msg.to_lowercase();
    if m.contains("unauthorized") {
        "O aparelho ainda não autorizou este computador: desbloqueie a tela e toque em \
         \"Permitir depuração USB\"."
            .into()
    } else if m.contains("no devices") {
        "Nenhum aparelho Android conectado. Confira o cabo e se a depuração USB está ligada.".into()
    } else if m.contains("more than one") {
        "Há mais de um aparelho conectado: escolha um na lista.".into()
    } else if m.contains("offline") {
        "O aparelho está offline: reconecte o cabo ou reinicie a depuração USB.".into()
    } else if m.contains("not found") {
        "Esse aparelho não está mais conectado: atualize a lista.".into()
    } else {
        format!("adb: {msg}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn le_a_lista_de_aparelhos() {
        let text = "R58M123ABC  device usb:1-1 product:a51 model:SM_A515F device:a51 transport_id:3\n\
                    emulator-5554 offline\n\
                    192.168.0.7:5555 unauthorized transport_id:4\n";
        let devices = parse_devices(text);
        assert_eq!(devices.len(), 3);
        assert_eq!(devices[0].model.as_deref(), Some("SM A515F"));
        assert_eq!(devices[0].label(), "SM A515F (R58M123ABC)");
        assert_eq!(devices[1].label(), "emulator-5554 · offline");
        assert_eq!(devices[2].label(), "192.168.0.7:5555 · não autorizado");
    }

    #[test]
    fn lista_vazia() {
        assert!(parse_devices("").is_empty());
    }

    #[test]
    fn traduz_recusas_comuns() {
        assert!(
            friendly("device unauthorized.\nThis adb server's $ADB_VENDOR_KEYS")
                .contains("autorizou")
        );
        assert!(friendly("no devices/emulators found").contains("Nenhum aparelho"));
        assert_eq!(friendly("algo novo"), "adb: algo novo");
    }
}
