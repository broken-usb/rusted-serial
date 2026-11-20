<p align="center">
  <img src="assets/icon.svg" alt="" width="96" height="96">
</p>

<h1 align="center">Rusted Serial</h1>

<p align="center">
  Terminal serial nativo, leve e rápido, escrito em Rust.<br>
  Para Arduino, microcontroladores (PIC, AVR, ESP…) e consoles de switches gerenciáveis.
</p>

![Rusted Serial conectado ao console de um switch](assets/screenshot.png)

## Recursos

- **Conexão completa:** porta, baud (300 a 921600), formato (8N1, 7E1…) e controle de fluxo (RTS/CTS, XON/XOFF).
- **Terminal:** horário em cada linha, o que você envia destacado, visão em texto ou HEX, rolagem automática e até 100 mil linhas sem perder velocidade.
- **Envio:** fim de linha configurável (CR, LF, CR+LF), envio em HEX (`AA 01 FF`) e histórico com ↑ ↓.
- **Senhas protegidas:** quando o dispositivo pede `Password:`, o que você digita não aparece na tela, não entra no histórico e é salvo como `[senha oculta]` no log.
- **Log:** salva a sessão em `~/rusted-serial-AAAAMMDD-HHMMSS.log`, legível só pelo seu usuário.
- **Cara de nativo:** segue o modo claro/escuro, a cor de destaque e as fontes do sistema (GNOME, KDE e Windows).
- **Portas virtuais:** aceita qualquer caminho em *Porta → Outra porta…* (`/dev/pts/3`, `/dev/rfcomm0`, `COM10`).

## Instalação

### Fedora

```bash
sudo dnf install rust cargo systemd-devel   # systemd-devel traz a libudev
sudo usermod -aG dialout $USER              # acesso às portas seriais (saia e entre na sessão)
```

> Se um Arduino conectado em `/dev/ttyACM*` reinicia ou mostra lixo logo ao plugar, é o
> ModemManager sondando a porta. Se você não usa modem 3G/4G:
> `sudo systemctl disable --now ModemManager`.

### Windows

Instale o Rust pelo [rustup](https://rustup.rs). Não há dependências extras.

Os executáveis de Linux e Windows também ficam disponíveis em cada execução do CI
(aba **Actions** → execução → **Artifacts**).

## Compilar e rodar

```bash
cargo run --release
```

Requer Rust 1.95 ou mais novo (exigência do egui 0.37).

## Testar sem hardware

Crie um par de portas virtuais ligadas entre si:

```bash
sudo dnf install socat
socat -d -d pty,raw,echo=0,link=/tmp/ttyA pty,raw,echo=0,link=/tmp/ttyB
```

No app, use *Porta → Outra porta…* → `/tmp/ttyA`. O que for escrito em `/tmp/ttyB`
(por exemplo `echo "olá" > /tmp/ttyB`) aparece no terminal.

## Estrutura

| Arquivo | O que faz |
|---|---|
| `src/serial.rs` | Conexão com a porta numa thread própria, que nunca bloqueia a interface |
| `src/app.rs` | Interface (egui) |
| `src/icons.rs` | Ícones vetoriais |
| `src/system.rs` | Lê tema, cor de destaque e fontes do sistema (uma vez, ao iniciar) |
| `src/theme.rs` | Paleta tingida com a cor de destaque, com contraste mínimo garantido |
| `src/fonts.rs` | Carrega as fontes do sistema |

## Créditos

Ícone do projeto: “Laptop” by [Delapouite](https://delapouite.com) under [CC BY 3.0](http://creativecommons.org/licenses/by/3.0/),
obtido e personalizado em [game-icons.net](https://game-icons.net).

## Licença

[MIT](LICENSE)
