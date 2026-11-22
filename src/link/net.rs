//! TCP cru e Telnet. Também conecta o SSH e repassa os bytes do ADB.
//!
//! E/S assíncrona (tokio) numa thread própria: a thread dorme até chegar um
//! comando da interface ou um dado da rede, sem varredura periódica.

use std::net::SocketAddr;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::telnet::{self, Telnet};
use super::{
    CONNECT_TIMEOUT, Command, Commands, Event, Exit, Link, block_on, describe_io, or_cancel,
};

pub(super) fn run(
    host: &str,
    port: u16,
    use_telnet: bool,
    cmds: &mut Commands,
    link: &Link,
) -> Exit {
    block_on(async {
        let stream = match connect(host, port, cmds).await {
            Ok(s) => s,
            Err(exit) => return exit,
        };
        // Mostra o IP quando o usuário digitou um nome.
        let detail = stream
            .peer_addr()
            .map(|a| a.ip().to_string())
            .ok()
            .filter(|ip| ip != host)
            .unwrap_or_default();
        if !link.emit(Event::Connected(detail)) {
            return Exit::UiGone;
        }
        pump(stream, cmds, link, use_telnet.then(Telnet::new)).await
    })
}

/// Resolve o nome e tenta cada endereço (IPv4/IPv6) até um aceitar. A
/// interface pode cancelar a qualquer momento.
pub(super) async fn connect(host: &str, port: u16, cmds: &mut Commands) -> Result<TcpStream, Exit> {
    let lookup = tokio::net::lookup_host((host, port));
    let addrs: Vec<SocketAddr> = match or_cancel(cmds, lookup).await? {
        Ok(found) => found.collect(),
        Err(e) => {
            return Err(Exit::Failed(format!(
                "Não encontrei o endereço {host}: {}",
                describe_io(&e)
            )));
        }
    };
    let mut last = format!("{host} não tem nenhum endereço");
    for addr in addrs {
        let attempt = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr));
        match or_cancel(cmds, attempt).await? {
            Ok(Ok(stream)) => {
                let _ = stream.set_nodelay(true); // só desempenho
                return Ok(stream);
            }
            Ok(Err(e)) => last = describe_io(&e),
            Err(_) => last = "sem resposta (tempo esgotado)".into(),
        }
    }
    Err(Exit::Failed(format!(
        "Não foi possível conectar a {host}:{port}: {last}"
    )))
}

/// Repassa bytes nos dois sentidos até alguém encerrar.
pub(super) async fn pump(
    stream: TcpStream,
    cmds: &mut Commands,
    link: &Link,
    mut telnet: Option<Telnet>,
) -> Exit {
    enum Step {
        Cmd(Option<Command>),
        Read(std::io::Result<usize>),
    }
    let (mut rd, mut wr) = stream.into_split();
    let mut buf = vec![0u8; 8192];
    loop {
        // As duas esperas podem ser canceladas sem perder dados.
        let step = tokio::select! {
            c = cmds.recv() => Step::Cmd(c),
            r = rd.read(&mut buf) => Step::Read(r),
        };
        match step {
            Step::Cmd(Some(Command::Write(data))) => {
                let bytes = match telnet {
                    Some(_) => telnet::encode(&data),
                    None => data,
                };
                if let Err(e) = wr.write_all(&bytes).await {
                    return Exit::Failed(format!("Erro de escrita: {}", describe_io(&e)));
                }
            }
            Step::Cmd(Some(Command::Answer(_))) => {}
            Step::Cmd(Some(Command::Disconnect)) => return Exit::Requested,
            Step::Cmd(None) => return Exit::UiGone,
            Step::Read(Ok(0)) => return Exit::Closed("O outro lado encerrou a conexão".into()),
            Step::Read(Ok(n)) => {
                let data = match &mut telnet {
                    Some(t) => {
                        let (data, reply) = t.decode(&buf[..n]);
                        if !reply.is_empty()
                            && let Err(e) = wr.write_all(&reply).await
                        {
                            return Exit::Failed(format!("Erro de escrita: {}", describe_io(&e)));
                        }
                        data
                    }
                    None => buf[..n].to_vec(),
                };
                if !data.is_empty() && !link.emit(Event::Data(data)) {
                    return Exit::UiGone;
                }
            }
            Step::Read(Err(e)) => {
                return Exit::Failed(format!("Erro de leitura: {}", describe_io(&e)));
            }
        }
    }
}
