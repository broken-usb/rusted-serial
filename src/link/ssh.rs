//! SSH em Rust puro (russh), sem depender do `ssh` instalado.
//!
//! Ordem, como o OpenSSH: conecta, confere a chave do servidor no
//! `~/.ssh/known_hosts` (servidor novo: pergunta ao usuário; chave mudada:
//! recusa), autentica e abre um shell com terminal ("xterm-256color").
//!
//! Autenticação: primeiro `none` (alguns equipamentos não pedem senha e é assim
//! que se descobre o que o servidor aceita), depois o agente SSH, as chaves
//! padrão de `~/.ssh`, teclado interativo e senha. Senhas e frases-senha são
//! pedidas à interface ([`Question::Secret`]) e nunca ficam guardadas.
//!
//! A chave do servidor é conferida logo depois da troca de chaves e antes de
//! qualquer credencial sair do computador.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::client::{self, AuthResult, Handle, KeyboardInteractiveAuthResponse};
use russh::keys::agent::AgentIdentity;
use russh::keys::{self, HashAlg, PrivateKeyWithHashAlg, PublicKey, PublicKeyOrCertificate};
use russh::{ChannelMsg, Disconnect, MethodKind, Preferred, cipher, kex, mac};

use super::{
    Answer, CONNECT_TIMEOUT, Command, Commands, Event, Exit, Link, Question, block_on, describe_io,
    net, or_cancel,
};

/// Servidor SSH e tamanho do terminal pedido (colunas × linhas).
#[derive(Clone, Debug)]
pub struct SshTarget {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub cols: u32,
    pub rows: u32,
}

impl SshTarget {
    fn label(&self) -> String {
        if self.port == 22 {
            format!("{}@{}", self.user, self.host)
        } else {
            format!("{}@{}:{}", self.user, self.host, self.port)
        }
    }
}

pub(super) fn run(target: &SshTarget, cmds: &mut Commands, link: &Link) -> Exit {
    block_on(async {
        match session(target, cmds, link).await {
            Ok(exit) | Err(exit) => exit,
        }
    })
}

/// Guarda a chave que o servidor apresentou na troca de chaves.
struct Client {
    server_key: Arc<Mutex<Option<PublicKey>>>,
}

impl client::Handler for Client {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        // Certificados de host não são pedidos (lista vazia em `Preferred`).
        let PublicKeyOrCertificate::PublicKey { key, .. } = key else {
            return Ok(false);
        };
        let Ok(mut slot) = self.server_key.lock() else {
            return Ok(false);
        };
        // Numa nova troca de chaves no meio da sessão, a chave não pode mudar.
        if slot.as_ref().is_some_and(|prev| prev != key) {
            return Ok(false);
        }
        *slot = Some(key.clone());
        Ok(true)
    }
}

async fn session(t: &SshTarget, cmds: &mut Commands, link: &Link) -> Result<Exit, Exit> {
    let config = Arc::new(client::Config {
        preferred: preferred(),
        keepalive_interval: Some(Duration::from_secs(30)),
        keepalive_max: 3,
        nodelay: true,
        ..Default::default()
    });
    let server_key = Arc::new(Mutex::new(None));
    let handler = Client {
        server_key: server_key.clone(),
    };
    let stream = net::connect(&t.host, t.port, cmds).await?;
    let connecting = tokio::time::timeout(
        CONNECT_TIMEOUT,
        client::connect_stream(config, stream, handler),
    );
    let mut session = match or_cancel(cmds, connecting).await? {
        Ok(Ok(handle)) => handle,
        Ok(Err(e)) => {
            return Err(Exit::Failed(format!(
                "Falha ao conectar em {}: {}",
                t.label(),
                describe(&e)
            )));
        }
        Err(_) => {
            return Err(Exit::Failed(format!(
                "Falha ao conectar em {}: sem resposta (tempo esgotado)",
                t.label()
            )));
        }
    };

    let key = server_key
        .lock()
        .ok()
        .and_then(|k| k.clone())
        .ok_or_else(|| Exit::Failed("O servidor não apresentou uma chave".into()))?;
    verify_host(t, &key, cmds, link).await?;

    let how = authenticate(&mut session, t, cmds, link).await?;

    let channel = or_cancel(cmds, session.channel_open_session())
        .await?
        .map_err(|e| {
            Exit::Failed(format!(
                "O servidor recusou abrir um shell: {}",
                describe(&e)
            ))
        })?;
    // Sem resposta esperada: equipamentos que não têm terminal só ignoram.
    let _ = channel
        .request_pty(false, "xterm-256color", t.cols, t.rows, 0, 0, &[])
        .await;
    channel.request_shell(false).await.map_err(|e| {
        Exit::Failed(format!(
            "O servidor recusou abrir um shell: {}",
            describe(&e)
        ))
    })?;
    if !link.emit(Event::Connected(format!("autenticado com {how}"))) {
        return Ok(Exit::UiGone);
    }

    let mut channel = channel;
    let mut exit_code = None;
    let exit = loop {
        enum Step {
            Cmd(Option<Command>),
            Msg(Option<ChannelMsg>),
        }
        let step = tokio::select! {
            c = cmds.recv() => Step::Cmd(c),
            m = channel.wait() => Step::Msg(m),
        };
        match step {
            Step::Cmd(Some(Command::Write(data))) => {
                if let Err(e) = channel.data_bytes(data).await {
                    break Exit::Failed(format!("Erro de escrita: {}", describe(&e)));
                }
            }
            Step::Cmd(Some(Command::Answer(_))) => {}
            Step::Cmd(Some(Command::Disconnect)) => break Exit::Requested,
            Step::Cmd(None) => break Exit::UiGone,
            Step::Msg(Some(ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. })) => {
                if !link.emit(Event::Data(data.to_vec())) {
                    break Exit::UiGone;
                }
            }
            Step::Msg(Some(ChannelMsg::ExitStatus { exit_status })) => {
                exit_code = Some(exit_status)
            }
            Step::Msg(Some(ChannelMsg::Close) | None) => {
                break Exit::Closed(match exit_code {
                    Some(0) | None => "O servidor encerrou a sessão".into(),
                    Some(c) => format!("O shell terminou com código {c}"),
                });
            }
            Step::Msg(Some(_)) => {}
        }
    };
    // Educado, mas sem segurar o fechamento se o servidor não responder.
    let bye = session.disconnect(Disconnect::ByApplication, "", "pt-BR");
    let _ = tokio::time::timeout(Duration::from_secs(1), bye).await;
    Ok(exit)
}

/// Algoritmos: os padrões seguros primeiro; no fim, os antigos que switches e
/// roteadores velhos ainda exigem (SHA-1, DH group1/14, AES-CBC). Só são usados
/// se o equipamento não aceitar nada melhor: a negociação escolhe o primeiro
/// da nossa lista que ele também tiver.
fn preferred() -> Preferred {
    let d = Preferred::DEFAULT;
    let mut kex = d.kex.to_vec();
    kex.extend([kex::DH_G14_SHA1, kex::DH_GEX_SHA1, kex::DH_G1_SHA1]);
    let mut cipher = d.cipher.to_vec();
    cipher.extend([
        cipher::AES_128_CBC,
        cipher::AES_192_CBC,
        cipher::AES_256_CBC,
    ]);
    let mut mac = d.mac.to_vec();
    mac.extend([mac::HMAC_SHA1_ETM, mac::HMAC_SHA1]);
    Preferred {
        kex: kex.into(),
        cipher: cipher.into(),
        mac: mac.into(),
        ..d
    }
}

/// Pergunta algo ao usuário e espera a resposta.
async fn ask(cmds: &mut Commands, link: &Link, question: Question) -> Result<Answer, Exit> {
    if !link.emit(Event::Ask(question)) {
        return Err(Exit::UiGone);
    }
    loop {
        match cmds.recv().await {
            Some(Command::Answer(a)) => return Ok(a),
            Some(Command::Disconnect) => return Err(Exit::Requested),
            None => return Err(Exit::UiGone),
            Some(Command::Write(_)) => {}
        }
    }
}

fn ssh_dir() -> Option<PathBuf> {
    std::env::home_dir().map(|h| h.join(".ssh"))
}

/// Mostra caminhos dentro da pasta do usuário como `~/...` (no Windows, o
/// caminho completo, que é o que as pessoas reconhecem lá).
fn tilde(path: &Path) -> String {
    if cfg!(windows) {
        return path.display().to_string();
    }
    match std::env::home_dir().and_then(|h| path.strip_prefix(h).ok().map(Path::to_path_buf)) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

async fn verify_host(
    t: &SshTarget,
    key: &PublicKey,
    cmds: &mut Commands,
    link: &Link,
) -> Result<(), Exit> {
    let dir = ssh_dir().ok_or_else(|| {
        Exit::Failed(
            "Não encontrei a pasta do usuário para guardar as chaves dos servidores".into(),
        )
    })?;
    let path = dir.join("known_hosts");
    let host = if t.port == 22 {
        t.host.clone()
    } else {
        format!("{}:{}", t.host, t.port)
    };
    match keys::check_known_hosts_path(&t.host, t.port, key, &path) {
        Ok(true) => Ok(()),
        Ok(false) => {
            let other_known = keys::known_hosts::known_host_keys_path(&t.host, t.port, &path)
                .is_ok_and(|k| !k.is_empty());
            let question = Question::HostKey {
                host: host.clone(),
                algorithm: key.algorithm().to_string(),
                fingerprint: key.fingerprint(HashAlg::Sha256).to_string(),
                other_known,
            };
            match ask(cmds, link, question).await? {
                Answer::Yes => {
                    create_private_dir(&dir);
                    keys::known_hosts::learn_known_hosts_path(&t.host, t.port, key, &path)
                        .map_err(|e| {
                            Exit::Failed(format!("Não consegui salvar em {}: {e}", tilde(&path)))
                        })?;
                    link.emit(Event::Info(format!(
                        "Chave de {host} salva em {}",
                        tilde(&path)
                    )));
                    Ok(())
                }
                _ => Err(Exit::Closed(
                    "Conexão cancelada: a chave do servidor não foi confirmada".into(),
                )),
            }
        }
        Err(keys::Error::KeyChanged { line }) => Err(Exit::Failed(format!(
            "ATENÇÃO: a chave de {host} MUDOU desde a última conexão (linha {line} de {}). \
             Pode ser um ataque, ou o equipamento foi reinstalado. Se tiver certeza de que é \
             seguro, apague essa linha e conecte de novo.",
            tilde(&path)
        ))),
        Err(e) => Err(Exit::Failed(format!(
            "Não consegui ler {}: {e}",
            tilde(&path)
        ))),
    }
}

/// `~/.ssh` com permissão 0700, como o OpenSSH espera.
fn create_private_dir(dir: &Path) {
    if dir.exists() {
        return;
    }
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    let _ = builder.create(dir); // se falhar, `learn` avisa o erro
}

/// Tenta os métodos de autenticação e diz qual funcionou.
async fn authenticate(
    session: &mut Handle<Client>,
    t: &SshTarget,
    cmds: &mut Commands,
    link: &Link,
) -> Result<String, Exit> {
    let user = t.user.as_str();
    let fail = |e: russh::Error| Exit::Failed(format!("Erro na autenticação: {}", describe(&e)));

    let mut allowed: Vec<MethodKind> = match or_cancel(cmds, session.authenticate_none(user))
        .await?
        .map_err(fail)?
    {
        AuthResult::Success => return Ok("acesso sem senha".into()),
        AuthResult::Failure {
            remaining_methods, ..
        } => remaining_methods.to_vec(),
    };
    let rsa_hash = session
        .best_supported_rsa_hash()
        .await
        .ok()
        .flatten()
        .flatten();
    let hash_for = |key: &PublicKey| key.algorithm().is_rsa().then_some(rsa_hash).flatten();

    // 1. Agente SSH (chaves já desbloqueadas pelo usuário).
    let mut offered: Vec<PublicKey> = Vec::new();
    if allowed.contains(&MethodKind::PublicKey)
        && let Some(mut agent) = connect_agent().await
        && let Ok(identities) = agent.request_identities().await
    {
        for id in identities {
            let AgentIdentity::PublicKey { key, comment } = id else {
                continue;
            };
            let hash = hash_for(&key);
            offered.push(key.clone());
            match or_cancel(
                cmds,
                session.authenticate_publickey_with(user, key, hash, &mut agent),
            )
            .await?
            {
                Ok(AuthResult::Success) => {
                    let name = if comment.is_empty() {
                        String::new()
                    } else {
                        format!(" ({comment})")
                    };
                    return Ok(format!("o agente SSH{name}"));
                }
                Ok(AuthResult::Failure {
                    remaining_methods, ..
                }) => allowed = remaining_methods.to_vec(),
                Err(_) => break, // agente com problema: segue para os outros métodos
            }
            if !allowed.contains(&MethodKind::PublicKey) {
                break;
            }
        }
    }

    // 2. Chaves padrão em ~/.ssh (as que o agente já ofereceu são puladas).
    if let Some(dir) = ssh_dir() {
        for name in ["id_ed25519", "id_ecdsa", "id_rsa"] {
            if !allowed.contains(&MethodKind::PublicKey) {
                break;
            }
            let path = dir.join(name);
            if !path.is_file() {
                continue;
            }
            let public = keys::load_public_key(path.with_extension("pub")).ok();
            if public.as_ref().is_some_and(|p| offered.contains(p)) {
                continue;
            }
            let Some(private) = load_key(&path, cmds, link).await? else {
                continue;
            };
            if offered.contains(private.public_key()) {
                continue;
            }
            offered.push(private.public_key().clone());
            let hash = hash_for(private.public_key());
            let key = PrivateKeyWithHashAlg::new(Arc::new(private), hash);
            match or_cancel(cmds, session.authenticate_publickey(user, key))
                .await?
                .map_err(fail)?
            {
                AuthResult::Success => return Ok(format!("a chave {}", tilde(&path))),
                AuthResult::Failure {
                    remaining_methods, ..
                } => allowed = remaining_methods.to_vec(),
            }
        }
    }

    // 3. Teclado interativo (senha via PAM, códigos de verificação).
    let mut typed_password = false;
    if allowed.contains(&MethodKind::KeyboardInteractive) {
        for _ in 0..3 {
            let mut reply = or_cancel(
                cmds,
                session.authenticate_keyboard_interactive_start(user, None),
            )
            .await?
            .map_err(fail)?;
            loop {
                match reply {
                    KeyboardInteractiveAuthResponse::Success => return Ok("senha".into()),
                    KeyboardInteractiveAuthResponse::Failure {
                        remaining_methods, ..
                    } => {
                        allowed = remaining_methods.to_vec();
                        break;
                    }
                    KeyboardInteractiveAuthResponse::InfoRequest {
                        name,
                        instructions,
                        prompts,
                    } => {
                        for text in [name, instructions] {
                            if !text.trim().is_empty() {
                                link.emit(Event::Info(text.trim().to_string()));
                            }
                        }
                        let mut answers = Vec::with_capacity(prompts.len());
                        for p in prompts {
                            let prompt = format!("{} · {}", t.label(), p.prompt.trim());
                            let question = if p.echo {
                                Question::Text { prompt }
                            } else {
                                Question::Secret { prompt }
                            };
                            match ask(cmds, link, question).await? {
                                Answer::Text(s) => answers.push(s),
                                _ => return Err(Exit::Closed("Autenticação cancelada".into())),
                            }
                            typed_password = true;
                        }
                        reply = or_cancel(
                            cmds,
                            session.authenticate_keyboard_interactive_respond(answers),
                        )
                        .await?
                        .map_err(fail)?;
                    }
                }
            }
            if !allowed.contains(&MethodKind::KeyboardInteractive) || !typed_password {
                break;
            }
            link.emit(Event::Info("Não foi aceito. Tente de novo.".into()));
        }
    }

    // 4. Senha (se o usuário já digitou no teclado interativo, não pede de novo).
    if allowed.contains(&MethodKind::Password) && !typed_password {
        for attempt in 0..3 {
            let prompt = if attempt == 0 {
                format!("Senha de {}:", t.label())
            } else {
                format!("Senha incorreta. Senha de {}:", t.label())
            };
            let password = match ask(cmds, link, Question::Secret { prompt }).await? {
                Answer::Text(p) => p,
                _ => return Err(Exit::Closed("Autenticação cancelada".into())),
            };
            match or_cancel(cmds, session.authenticate_password(user, password))
                .await?
                .map_err(fail)?
            {
                AuthResult::Success => return Ok("senha".into()),
                AuthResult::Failure {
                    remaining_methods, ..
                } => allowed = remaining_methods.to_vec(),
            }
            if !allowed.contains(&MethodKind::Password) {
                break;
            }
        }
    }

    let methods: Vec<&str> = allowed.iter().map(<&str>::from).collect();
    Err(Exit::Failed(format!(
        "O servidor recusou a autenticação de {} (métodos aceitos: {})",
        t.label(),
        if methods.is_empty() {
            "nenhum".into()
        } else {
            methods.join(", ")
        }
    )))
}

/// Carrega uma chave privada; se tiver frase-senha, pede ao usuário (até 3 vezes).
/// `None`: pular esta chave (formato não suportado ou o usuário deixou em branco).
async fn load_key(
    path: &Path,
    cmds: &mut Commands,
    link: &Link,
) -> Result<Option<keys::PrivateKey>, Exit> {
    match keys::load_secret_key(path, None) {
        Ok(k) => return Ok(Some(k)),
        Err(keys::Error::KeyIsEncrypted) => {}
        Err(_) => return Ok(None),
    }
    for attempt in 0..3 {
        let prompt = if attempt == 0 {
            format!("Frase-senha da chave {} (vazio pula):", tilde(path))
        } else {
            format!("Frase-senha incorreta. Chave {} (vazio pula):", tilde(path))
        };
        match ask(cmds, link, Question::Secret { prompt }).await? {
            Answer::Text(p) if !p.is_empty() => {
                if let Ok(k) = keys::load_secret_key(path, Some(&p)) {
                    return Ok(Some(k));
                }
            }
            _ => return Ok(None),
        }
    }
    Ok(None)
}

#[cfg(unix)]
async fn connect_agent() -> Option<keys::agent::client::AgentClient<tokio::net::UnixStream>> {
    keys::agent::client::AgentClient::connect_env().await.ok()
}

/// Windows: agente do OpenSSH (serviço ssh-agent).
#[cfg(windows)]
async fn connect_agent()
-> Option<keys::agent::client::AgentClient<tokio::net::windows::named_pipe::NamedPipeClient>> {
    keys::agent::client::AgentClient::connect_named_pipe(r"\\.\pipe\openssh-ssh-agent")
        .await
        .ok()
}

#[cfg(not(any(unix, windows)))]
async fn connect_agent() -> Option<keys::agent::client::AgentClient<tokio::io::DuplexStream>> {
    None
}

fn describe(e: &russh::Error) -> String {
    match e {
        russh::Error::IO(io) => describe_io(io),
        russh::Error::ConnectionTimeout | russh::Error::KeepaliveTimeout => {
            "sem resposta (tempo esgotado)".into()
        }
        russh::Error::UnknownKey => "chave do servidor recusada".into(),
        russh::Error::NoCommonAlgo { kind, theirs, .. } => format!(
            "nenhum algoritmo em comum com o servidor ({kind:?}; ele oferece: {})",
            theirs.join(", ")
        ),
        other => other.to_string(),
    }
}
