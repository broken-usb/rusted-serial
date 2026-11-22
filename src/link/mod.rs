//! Conexões: serial, TCP, Telnet, SSH e ADB. Não sabe nada sobre a interface.
//!
//! Cada conexão roda numa thread própria e conversa com a interface por dois
//! canais: comandos descem ([`Command`]) e eventos sobem ([`Event`]). Abrir
//! nunca bloqueia a interface: a thread resolve o nome, conecta, autentica e só
//! então avisa [`Event::Connected`]. Quando precisa de algo do usuário (senha,
//! confirmar a chave de um servidor SSH novo), manda um [`Event::Ask`] e espera
//! o [`Command::Answer`].

mod adb;
mod net;
mod serial;
mod ssh;
mod telnet;

use std::future::Future;
use std::io;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

pub use adb::{Device as AdbDevice, list_devices as list_adb_devices};
pub use serial::{DataBits, FlowControl, Parity, PortConfig, StopBits, list_ports};
pub use ssh::SshTarget;

/// Tempo máximo de cada leitura/escrita na serial: a cada volta a thread
/// atende a interface. (As conexões de rede usam E/S assíncrona.)
const IO_TIMEOUT: Duration = Duration::from_millis(10);
/// Tempo máximo para abrir uma conexão de rede.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Para onde conectar.
pub enum Target {
    Serial(PortConfig),
    /// Bytes crus por TCP (ser2net, ESP32 via Wi-Fi, conversor serial-Ethernet).
    Tcp {
        host: String,
        port: u16,
    },
    Telnet {
        host: String,
        port: u16,
    },
    Ssh(SshTarget),
    /// Shell de um aparelho Android pelo servidor do adb. `None`: o único conectado.
    Adb {
        device: Option<String>,
    },
}

/// Comandos que a interface envia para a thread da conexão.
pub enum Command {
    Write(Vec<u8>),
    Answer(Answer),
    Disconnect,
}

/// Resposta a uma [`Question`].
pub enum Answer {
    Text(String),
    Yes,
    No,
}

/// Algo que só o usuário pode responder.
#[derive(Clone, Debug)]
pub enum Question {
    /// Senha ou frase-senha: o campo de envio fica mascarado.
    Secret { prompt: String },
    /// Texto visível (ex.: "Código de verificação" com eco).
    Text { prompt: String },
    /// Primeira conexão com um servidor SSH: confirmar a chave dele.
    HostKey {
        host: String,
        algorithm: String,
        fingerprint: String,
        /// Já existe outra chave (de outro tipo) salva para esse servidor.
        other_known: bool,
    },
}

/// Eventos que a thread devolve para a interface.
pub enum Event {
    /// Pronto para enviar e receber. O texto detalha (ex.: como autenticou).
    Connected(String),
    /// Mensagem de andamento para mostrar no terminal.
    Info(String),
    Data(Vec<u8>),
    Ask(Question),
    Error(String),
    Disconnected,
}

/// Uma conexão. Ao ser descartada, pede para a thread encerrar.
pub struct Connection {
    cmd_tx: UnboundedSender<Command>,
    evt_rx: Receiver<Event>,
    /// Só a serial espera a thread terminar: a porta precisa estar fechada
    /// antes de ser aberta de novo. As de rede podem estar presas numa busca
    /// de DNS e terminam sozinhas logo depois.
    thread: Option<JoinHandle<()>>,
}

impl Connection {
    /// Inicia a thread da conexão. `notify` é chamado (na thread da conexão)
    /// sempre que há eventos novos; a interface usa isso para se redesenhar.
    ///
    /// # Errors
    /// Só falha se o sistema não conseguir criar a thread.
    pub fn open(target: Target, notify: impl Fn() + Send + Sync + 'static) -> io::Result<Self> {
        let (cmd_tx, mut cmd_rx) = unbounded_channel();
        let (evt_tx, evt_rx) = mpsc::channel();
        let link = Link {
            evt: evt_tx,
            notify: Arc::new(notify),
        };
        let join = matches!(target, Target::Serial(_));
        let name = match &target {
            Target::Serial(c) => format!("serial-{}", c.name),
            Target::Tcp { .. } => "tcp".into(),
            Target::Telnet { .. } => "telnet".into(),
            Target::Ssh(_) => "ssh".into(),
            Target::Adb { .. } => "adb".into(),
        };
        let thread = thread::Builder::new().name(name).spawn(move || {
            let exit = match target {
                Target::Serial(config) => serial::run(&config, &mut cmd_rx, &link),
                Target::Tcp { host, port } => net::run(&host, port, false, &mut cmd_rx, &link),
                Target::Telnet { host, port } => net::run(&host, port, true, &mut cmd_rx, &link),
                Target::Ssh(target) => ssh::run(&target, &mut cmd_rx, &link),
                Target::Adb { device } => adb::run(device.as_deref(), &mut cmd_rx, &link),
            };
            link.finish(exit);
        })?;
        Ok(Self {
            cmd_tx,
            evt_rx,
            thread: join.then_some(thread),
        })
    }

    /// Enfileira bytes para envio (não bloqueia).
    pub fn send(&self, data: Vec<u8>) {
        // Se a thread já terminou, o evento `Disconnected` avisa a interface.
        let _ = self.cmd_tx.send(Command::Write(data));
    }

    pub fn answer(&self, answer: Answer) {
        let _ = self.cmd_tx.send(Command::Answer(answer));
    }

    /// Retorna todos os eventos pendentes sem bloquear.
    pub fn poll(&self) -> Vec<Event> {
        self.evt_rx.try_iter().collect()
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(Command::Disconnect);
        if let Some(t) = self.thread.take() {
            // Serial: termina em no máximo ~2 × IO_TIMEOUT (nunca bloqueia).
            let _ = t.join();
        }
    }
}

/// Por que a thread parou.
enum Exit {
    /// A interface pediu para desconectar.
    Requested,
    /// A interface foi fechada (canal de comandos caiu).
    UiGone,
    /// O outro lado encerrou normalmente (ou o usuário cancelou um passo).
    Closed(String),
    /// Erro (porta sumiu, conexão recusada, autenticação negada...).
    Failed(String),
}

/// Lado da thread: manda eventos e acorda a interface.
#[derive(Clone)]
struct Link {
    evt: Sender<Event>,
    notify: Arc<dyn Fn() + Send + Sync>,
}

impl Link {
    /// Retorna `false` se a interface já fechou.
    fn emit(&self, event: Event) -> bool {
        let ok = self.evt.send(event).is_ok();
        (self.notify)();
        ok
    }

    fn finish(&self, exit: Exit) {
        match exit {
            Exit::UiGone => return,
            Exit::Requested => {}
            Exit::Closed(msg) => {
                self.emit(Event::Info(msg));
            }
            Exit::Failed(msg) => {
                self.emit(Event::Error(msg));
            }
        }
        self.emit(Event::Disconnected);
    }
}

type Commands = UnboundedReceiver<Command>;

/// Mensagem amigável para erros comuns de rede.
fn describe_io(e: &io::Error) -> String {
    match e.kind() {
        io::ErrorKind::ConnectionRefused => "conexão recusada (nada escutando nessa porta?)".into(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => {
            "sem resposta (tempo esgotado)".into()
        }
        io::ErrorKind::HostUnreachable | io::ErrorKind::NetworkUnreachable => {
            "endereço inalcançável (confira a rede)".into()
        }
        io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted => {
            "a conexão caiu".into()
        }
        _ => e.to_string(),
    }
}

/// Roda `fut` num runtime assíncrono de uma thread só (a da conexão).
fn block_on(fut: impl Future<Output = Exit>) -> Exit {
    match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt.block_on(fut),
        Err(e) => Exit::Failed(format!("Erro interno ao iniciar a conexão: {e}")),
    }
}

/// Executa `fut`, mas desiste se a interface pedir para desconectar.
async fn or_cancel<T>(cmds: &mut Commands, fut: impl Future<Output = T>) -> Result<T, Exit> {
    tokio::pin!(fut);
    loop {
        tokio::select! {
            out = &mut fut => return Ok(out),
            cmd = cmds.recv() => match cmd {
                Some(Command::Disconnect) => return Err(Exit::Requested),
                None => return Err(Exit::UiGone),
                Some(_) => {} // escrita ou resposta fora de hora: ignora
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::time::Instant;

    /// Junta os eventos até `done` dizer que chegou o esperado (ou 10 s).
    fn collect(conn: &Connection, done: impl Fn(&[Event]) -> bool) -> Vec<Event> {
        let start = Instant::now();
        let mut events = Vec::new();
        while start.elapsed() < Duration::from_secs(10) {
            events.extend(conn.poll());
            if done(&events) {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        events
    }

    fn data(events: &[Event]) -> Vec<u8> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::Data(d) => Some(d.as_slice()),
                _ => None,
            })
            .flatten()
            .copied()
            .collect()
    }

    fn connected(events: &[Event]) -> bool {
        events.iter().any(|e| matches!(e, Event::Connected(_)))
    }

    /// Servidor de um cliente só, rodando `serve` numa thread.
    fn server(serve: impl FnOnce(TcpStream) + Send + 'static) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                serve(stream);
            }
        });
        port
    }

    fn read_until(stream: &mut TcpStream, end: &[u8]) -> Vec<u8> {
        let mut got = Vec::new();
        let mut byte = [0u8; 1];
        while !got.ends_with(end) && stream.read(&mut byte).unwrap_or(0) == 1 {
            got.push(byte[0]);
        }
        got
    }

    #[test]
    fn tcp_vai_e_volta() {
        let port = server(|mut s| {
            s.write_all(b"ola\n").unwrap();
            let line = read_until(&mut s, b"\n");
            s.write_all(&line.to_ascii_uppercase()).unwrap();
            thread::sleep(Duration::from_millis(200));
        });
        let target = Target::Tcp {
            host: "127.0.0.1".into(),
            port,
        };
        let conn = Connection::open(target, || {}).unwrap();
        let events = collect(&conn, |e| data(e) == b"ola\n");
        assert!(connected(&events));
        conn.send(b"ping\n".to_vec());
        let events = collect(&conn, |e| data(e) == b"PING\n");
        assert_eq!(data(&events), b"PING\n");
        // O servidor fecha: a conexão avisa e termina.
        let events = collect(&conn, |e| {
            e.iter().any(|e| matches!(e, Event::Disconnected))
        });
        assert!(events.iter().any(|e| matches!(e, Event::Info(_))));
    }

    #[test]
    fn telnet_negocia_e_entrega_so_os_dados() {
        let port = server(|mut s| {
            // WILL ECHO, DO TTYPE, depois o prompt
            s.write_all(&[255, 251, 1, 255, 253, 24]).unwrap();
            s.write_all(b"login: ").unwrap();
            let reply = read_until(&mut s, &[255, 252, 24]);
            assert_eq!(reply, [255, 253, 1, 255, 252, 24]);
            let typed = read_until(&mut s, b"\r\n");
            s.write_all(&typed).unwrap(); // eco do servidor
            thread::sleep(Duration::from_millis(300));
        });
        let target = Target::Telnet {
            host: "localhost".into(),
            port,
        };
        let conn = Connection::open(target, || {}).unwrap();
        let events = collect(&conn, |e| data(e) == b"login: ");
        assert_eq!(data(&events), b"login: ");
        conn.send(vec![b'a', 0xFF, b'\r', b'\n']);
        // 0xFF foi enviado escapado (FF FF) e voltou do eco como um 0xFF só.
        let events = collect(&conn, |e| data(e).ends_with(b"\r\n"));
        assert_eq!(data(&events), [b'a', 0xFF, b'\r', b'\n']);
    }

    #[test]
    fn conexao_recusada_avisa_sem_conectar() {
        // Porta de um listener que acabou de fechar: nada escutando.
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let target = Target::Tcp {
            host: "127.0.0.1".into(),
            port,
        };
        let conn = Connection::open(target, || {}).unwrap();
        let events = collect(&conn, |e| {
            e.iter().any(|e| matches!(e, Event::Disconnected))
        });
        assert!(!connected(&events));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Error(m) if m.contains("recusada")))
        );
    }

    #[test]
    fn cancelar_durante_a_conexao_nao_trava() {
        // 10.255.255.1 não responde: a conexão fica esperando.
        let target = Target::Tcp {
            host: "10.255.255.1".into(),
            port: 9,
        };
        let conn = Connection::open(target, || {}).unwrap();
        let start = Instant::now();
        drop(conn);
        assert!(start.elapsed() < Duration::from_millis(500));
    }
}
