//! Camada serial: não sabe nada sobre a interface gráfica.
//! Roda numa thread própria e conversa com a UI por canais (mpsc).

use std::io::{Read, Write};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub use serialport::{DataBits, FlowControl, Parity, StopBits};

/// Configuração da porta (o famoso "9600 8N1").
#[derive(Clone, Debug)]
pub struct PortConfig {
    pub name: String,
    pub baud_rate: u32,
    pub data_bits: DataBits,
    pub parity: Parity,
    pub stop_bits: StopBits,
    pub flow_control: FlowControl,
}

impl Default for PortConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            baud_rate: 9600,
            data_bits: DataBits::Eight,
            parity: Parity::None,
            stop_bits: StopBits::One,
            flow_control: FlowControl::None,
        }
    }
}

/// Comandos que a UI envia para a thread serial.
pub enum Command {
    Write(Vec<u8>),
    Disconnect,
}

/// Eventos que a thread serial devolve para a UI.
pub enum Event {
    Data(Vec<u8>),
    Error(String),
    Disconnected,
}

/// Handle de uma conexão ativa.
pub struct Connection {
    pub config: PortConfig,
    cmd_tx: Sender<Command>,
    evt_rx: Receiver<Event>,
    thread: Option<JoinHandle<()>>,
}

/// Lista as portas disponíveis (/dev/ttyUSB0, /dev/ttyACM0, COM3...).
pub fn list_ports() -> Vec<String> {
    match serialport::available_ports() {
        Ok(ports) => ports.into_iter().map(|p| p.port_name).collect(),
        Err(_) => Vec::new(),
    }
}

impl Connection {
    /// Abre a porta e inicia a thread de leitura/escrita.
    /// `notify` é chamado sempre que chegam dados (usado para acordar a UI).
    pub fn open(
        config: PortConfig,
        notify: impl Fn() + Send + 'static,
    ) -> Result<Self, serialport::Error> {
        let mut port = serialport::new(&config.name, config.baud_rate)
            .data_bits(config.data_bits)
            .parity(config.parity)
            .stop_bits(config.stop_bits)
            .flow_control(config.flow_control)
            // Timeout curto: a thread nunca fica presa no read()
            // e consegue checar comandos da UI com frequência.
            .timeout(Duration::from_millis(10))
            .open()?;

        let (cmd_tx, cmd_rx) = mpsc::channel::<Command>();
        let (evt_tx, evt_rx) = mpsc::channel::<Event>();

        let thread = thread::Builder::new()
            .name(format!("serial-{}", config.name))
            .spawn(move || {
                let mut buf = [0u8; 4096];
                'main: loop {
                    // 1) Processa todos os comandos pendentes da UI.
                    loop {
                        match cmd_rx.try_recv() {
                            Ok(Command::Write(data)) => {
                                if let Err(e) = port.write_all(&data).and_then(|_| port.flush()) {
                                    let _ =
                                        evt_tx.send(Event::Error(format!("Erro de escrita: {e}")));
                                    notify();
                                    break 'main;
                                }
                            }
                            Ok(Command::Disconnect) | Err(TryRecvError::Disconnected) => {
                                break 'main;
                            }
                            Err(TryRecvError::Empty) => break,
                        }
                    }

                    // 2) Lê o que tiver chegado na porta.
                    match port.read(&mut buf) {
                        Ok(0) => {}
                        Ok(n) => {
                            if evt_tx.send(Event::Data(buf[..n].to_vec())).is_err() {
                                break 'main; // UI foi fechada
                            }
                            notify();
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                        Err(e) => {
                            // Ex.: cabo USB desconectado.
                            let _ = evt_tx.send(Event::Error(format!("Erro de leitura: {e}")));
                            notify();
                            break 'main;
                        }
                    }
                }
                let _ = evt_tx.send(Event::Disconnected);
                notify();
            })
            .expect("falha ao criar thread serial");

        Ok(Self {
            config,
            cmd_tx,
            evt_rx,
            thread: Some(thread),
        })
    }

    pub fn send(&self, data: Vec<u8>) {
        let _ = self.cmd_tx.send(Command::Write(data));
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
            let _ = t.join();
        }
    }
}
