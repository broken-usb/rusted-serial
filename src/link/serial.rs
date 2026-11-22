//! Porta serial.
//!
//! A thread nunca bloqueia por mais que [`IO_TIMEOUT`]: a cada volta ela atende
//! os comandos, escreve o que estiver pendente e lê o que chegou. Assim um
//! dispositivo que pede pausa (XOFF/CTS) não trava o envio nem o "Desconectar".

use std::collections::VecDeque;
use std::io::{self, Read, Write};

use serialport::{ClearBuffer, SerialPort};
pub use serialport::{DataBits, FlowControl, Parity, StopBits};
use tokio::sync::mpsc::error::TryRecvError;

use super::{Command, Commands, Event, Exit, IO_TIMEOUT, Link};

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

/// Lista as portas disponíveis (/dev/ttyUSB0, /dev/ttyACM0, COM3...).
pub fn list_ports() -> Vec<String> {
    serialport::available_ports()
        .map(|ports| ports.into_iter().map(|p| p.port_name).collect())
        .unwrap_or_default()
}

/// Abre a porta e atende a interface até desconectar.
pub(super) fn run(config: &PortConfig, cmds: &mut Commands, link: &Link) -> Exit {
    let port = match serialport::new(&config.name, config.baud_rate)
        .data_bits(config.data_bits)
        .parity(config.parity)
        .stop_bits(config.stop_bits)
        .flow_control(config.flow_control)
        .timeout(IO_TIMEOUT)
        .open()
    {
        Ok(port) => port,
        Err(e) => return Exit::Failed(format!("Falha ao abrir {}: {e}", config.name)),
    };
    if !link.emit(Event::Connected(String::new())) {
        return Exit::UiGone;
    }
    Worker {
        port,
        cmds,
        link,
        outgoing: VecDeque::new(),
    }
    .run()
}

struct Worker<'a> {
    port: Box<dyn SerialPort>,
    cmds: &'a mut Commands,
    link: &'a Link,
    /// Bytes aguardando envio (o dispositivo pode ter pedido pausa).
    outgoing: VecDeque<u8>,
}

impl Worker<'_> {
    fn run(mut self) -> Exit {
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
        exit
    }

    /// Atende todos os comandos pendentes da interface.
    fn handle_commands(&mut self) -> Result<(), Exit> {
        loop {
            match self.cmds.try_recv() {
                Ok(Command::Write(data)) => self.outgoing.extend(data),
                Ok(Command::Answer(_)) => {}
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
                if !self.link.emit(Event::Data(buf[..n].to_vec())) {
                    return Err(Exit::UiGone);
                }
                Ok(())
            }
            Err(e) if e.kind() == io::ErrorKind::TimedOut => Ok(()),
            Err(e) => Err(Exit::Failed(format!("Erro de leitura: {e}"))),
        }
    }
}
