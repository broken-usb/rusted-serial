//! Camada serial: não sabe nada sobre a interface gráfica.
//!
//! Cada conexão roda numa thread própria e conversa com a interface por dois
//! canais (`mpsc`): comandos descem ([`Command`]) e eventos sobem ([`Event`]).
//! A thread nunca bloqueia por mais que [`IO_TIMEOUT`]: a cada volta ela atende
//! os comandos, escreve o que estiver pendente e lê o que chegou. Assim um
//! dispositivo que pede pausa (XOFF/CTS) não trava o envio nem o "Desconectar".

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serialport::{ClearBuffer, SerialPort};
pub use serialport::{DataBits, FlowControl, Parity, StopBits};

/// Tempo máximo de cada leitura/escrita na porta.
const IO_TIMEOUT: Duration = Duration::from_millis(10);

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

/// Comandos que a interface envia para a thread serial.
pub enum Command {
    Write(Vec<u8>),
    Disconnect,
}

/// Eventos que a thread serial devolve para a interface.
pub enum Event {
    Data(Vec<u8>),
    Error(String),
    Disconnected,
}

/// Uma conexão aberta. Ao ser descartada, encerra a thread e fecha a porta.
pub struct Connection {
    cmd_tx: Sender<Command>,
    evt_rx: Receiver<Event>,
    thread: Option<JoinHandle<()>>,
}

/// Lista as portas disponíveis (/dev/ttyUSB0, /dev/ttyACM0, COM3...).
pub fn list_ports() -> Vec<String> {
    serialport::available_ports()
        .map(|ports| ports.into_iter().map(|p| p.port_name).collect())
        .unwrap_or_default()
}

impl Connection {
    /// Abre a porta e inicia a thread de leitura/escrita.
    ///
    /// `notify` é chamado (na thread serial) sempre que há eventos novos;
    /// a interface usa isso para se redesenhar.
    ///
    /// # Errors
    /// Falha se a porta não puder ser aberta (inexistente, ocupada, sem
    /// permissão) ou se o sistema não conseguir criar a thread.
    pub fn open(
        config: &PortConfig,
        notify: impl Fn() + Send + 'static,
    ) -> Result<Self, serialport::Error> {
        let port = serialport::new(&config.name, config.baud_rate)
            .data_bits(config.data_bits)
            .parity(config.parity)
            .stop_bits(config.stop_bits)
            .flow_control(config.flow_control)
            .timeout(IO_TIMEOUT)
            .open()?;

        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (evt_tx, evt_rx) = mpsc::channel();
        let worker = Worker {
            port,
            cmd_rx,
            evt_tx,
            notify,
            outgoing: VecDeque::new(),
        };

        let thread = thread::Builder::new()
            .name(format!("serial-{}", config.name))
            .spawn(move || worker.run())?;

        Ok(Self {
            cmd_tx,
            evt_rx,
            thread: Some(thread),
        })
    }

    /// Enfileira bytes para envio (não bloqueia).
    pub fn send(&self, data: Vec<u8>) {
        // Se a thread já terminou, o evento `Disconnected` avisa a interface.
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
            // Termina em no máximo ~2 × IO_TIMEOUT: a thread nunca bloqueia.
            let _ = t.join();
        }
    }
}

/// Por que a thread serial parou.
enum Exit {
    /// A interface pediu para desconectar.
    Requested,
    /// A interface foi fechada (canal de comandos caiu).
    UiGone,
    /// Erro na porta (ex.: cabo USB removido).
    Failed(String),
}

struct Worker<N> {
    port: Box<dyn SerialPort>,
    cmd_rx: Receiver<Command>,
    evt_tx: Sender<Event>,
    notify: N,
    /// Bytes aguardando envio (o dispositivo pode ter pedido pausa).
    outgoing: VecDeque<u8>,
}

impl<N: Fn()> Worker<N> {
    fn run(mut self) {
        let mut buf = [0u8; 4096];
        let exit = loop {
            if let Err(exit) = self.handle_commands() {
                break exit;
            }
            if let Err(exit) = self.write_pending() {
                break exit;
            }
            if let Err(exit) = self.read_incoming(&mut buf) {
                break exit;
            }
        };

        // Descarta o que não foi enviado: senão o `close()` da porta pode esperar
        // o dispositivo liberar o envio (até 30 s em adaptadores USB no Linux).
        let _ = self.port.clear(ClearBuffer::Output);

        // Os `send` abaixo só falham se a interface já fechou: não há a quem avisar.
        match exit {
            Exit::UiGone => return,
            Exit::Failed(msg) => {
                let _ = self.evt_tx.send(Event::Error(msg));
            }
            Exit::Requested => {}
        }
        let _ = self.evt_tx.send(Event::Disconnected);
        (self.notify)();
    }

    /// Atende todos os comandos pendentes da interface.
    fn handle_commands(&mut self) -> Result<(), Exit> {
        loop {
            match self.cmd_rx.try_recv() {
                Ok(Command::Write(data)) => self.outgoing.extend(data),
                Ok(Command::Disconnect) => return Err(Exit::Requested),
                Err(TryRecvError::Disconnected) => return Err(Exit::UiGone),
                Err(TryRecvError::Empty) => return Ok(()),
            }
        }
    }

    /// Envia o que der dos bytes pendentes, sem esperar além de `IO_TIMEOUT`.
    fn write_pending(&mut self) -> Result<(), Exit> {
        let (chunk, _) = self.outgoing.as_slices();
        if chunk.is_empty() {
            return Ok(());
        }
        match self.port.write(chunk) {
            Ok(n) => {
                self.outgoing.drain(..n);
                Ok(())
            }
            // Dispositivo pediu pausa (XOFF/CTS) ou buffer cheio: tenta na próxima volta.
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) =>
            {
                Ok(())
            }
            Err(e) => Err(Exit::Failed(format!("Erro de escrita: {e}"))),
        }
    }

    /// Lê o que chegou na porta e repassa para a interface.
    fn read_incoming(&mut self, buf: &mut [u8]) -> Result<(), Exit> {
        match self.port.read(buf) {
            Ok(0) => Ok(()),
            Ok(n) => {
                if self.evt_tx.send(Event::Data(buf[..n].to_vec())).is_err() {
                    return Err(Exit::UiGone);
                }
                (self.notify)();
                Ok(())
            }
            Err(e) if e.kind() == io::ErrorKind::TimedOut => Ok(()),
            Err(e) => Err(Exit::Failed(format!("Erro de leitura: {e}"))),
        }
    }
}
