//! Ícone da janela no Wayland (só Linux).
//!
//! No Wayland o app não consegue entregar o ícone para a barra de título: o
//! compositor (KWin, Mutter, COSMIC...) procura um arquivo `<app_id>.desktop`
//! e usa o `Icon=` dele. Os pacotes .deb/.rpm instalam esse arquivo; o
//! AppImage, o .tar.gz e o `cargo run` não, e a janela ficava com o ícone
//! genérico do Wayland.
//!
//! Quando nenhum pacote instalou o arquivo, o app cria um oculto
//! (`NoDisplay=true`, não aparece no menu) em `~/.local/share/applications`,
//! com o ícone em `~/.local/share/rusted-serial/`. Regras:
//! - nunca mexe em um `rusted-serial.desktop` que não foi ele que criou;
//! - se depois um pacote instalar o app, apaga o seu (vale o do pacote);
//! - só regrava quando algo muda (ex.: o AppImage mudou de pasta).
//!
//! `Icon=` leva o caminho absoluto do PNG: o KWin abre o arquivo direto, sem
//! cache de tema de ícones, então funciona já na primeira vez que o app abre.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::{env, fs, io};

/// Marca os arquivos criados por nós (para nunca apagar os de outros).
const MARKER: &str = "X-Rusted-Serial-Auto=true";

/// Garante o arquivo antes de a janela abrir. Erros são ignorados: no pior
/// caso a janela fica com o ícone genérico, como era antes.
pub fn ensure(app_id: &str, icon_png: &[u8]) {
    // Flatpak e Snap exportam o .desktop do jeito deles.
    if env::var_os("FLATPAK_ID").is_some() || env::var_os("SNAP").is_some() {
        return;
    }
    let Some(data_home) = data_home(env::var_os("XDG_DATA_HOME"), env::var_os("HOME")) else {
        return;
    };
    let data_dirs = data_dirs(env::var_os("XDG_DATA_DIRS"));
    // Dentro do AppImage, `current_exe` aponta para a montagem temporária.
    let exe = env::var_os("APPIMAGE")
        .map(PathBuf::from)
        .or_else(|| env::current_exe().ok());
    let _ = sync(app_id, &data_home, &data_dirs, exe.as_deref(), icon_png);
}

fn sync(
    app_id: &str,
    data_home: &Path,
    data_dirs: &[PathBuf],
    exe: Option<&Path>,
    icon_png: &[u8],
) -> io::Result<()> {
    let file_name = format!("{app_id}.desktop");
    let ours = data_home.join("applications").join(&file_name);
    let current = match fs::read_to_string(&ours) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    if current.as_deref().is_some_and(|text| !is_ours(text)) {
        return Ok(()); // do usuário ou de outra ferramenta: não mexe
    }

    let installed = data_dirs
        .iter()
        .filter(|dir| dir.as_path() != data_home)
        .any(|dir| dir.join("applications").join(&file_name).is_file());
    if installed {
        if current.is_some() {
            fs::remove_file(&ours)?;
        }
        return Ok(());
    }

    let Some(exe) = exe else { return Ok(()) };
    let icon = data_home.join(app_id).join(format!("{app_id}.png"));
    let Some(entry) = render(app_id, exe, &icon) else {
        return Ok(()); // caminho que o formato .desktop não representa
    };
    if fs::read(&icon).ok().as_deref() != Some(icon_png) {
        write_atomic(&icon, icon_png)?;
    }
    if current.as_deref() != Some(entry.as_str()) {
        write_atomic(&ours, entry.as_bytes())?;
    }
    Ok(())
}

fn is_ours(text: &str) -> bool {
    text.lines().any(|line| line.trim() == MARKER)
}

fn render(app_id: &str, exe: &Path, icon: &Path) -> Option<String> {
    let exec = exec_arg(exe.to_str()?)?;
    let icon = string_value(icon.to_str()?)?;
    Some(format!(
        "# Criado pelo Rusted Serial para mostrar o ícone na barra de título no Wayland.\n\
         # Não aparece no menu. Pode apagar: é recriado quando o app abre.\n\
         [Desktop Entry]\n\
         Type=Application\n\
         Name=Rusted Serial\n\
         Exec={exec}\n\
         Icon={icon}\n\
         Terminal=false\n\
         NoDisplay=true\n\
         StartupWMClass={app_id}\n\
         {MARKER}\n"
    ))
}

/// Um argumento de `Exec=`, sempre entre aspas. Dentro das aspas, `"`, `` ` ``,
/// `$` e `\` levam `\` antes; depois vale o escape geral de strings do
/// formato (`\` vira `\\`), por isso o `\` literal vira quatro. `%` vira `%%`.
fn exec_arg(path: &str) -> Option<String> {
    if path.chars().any(char::is_control) {
        return None;
    }
    let mut out = String::from("\"");
    for c in path.chars() {
        match c {
            '"' | '`' | '$' => {
                out.push_str(r"\\");
                out.push(c);
            }
            '\\' => out.push_str(r"\\\\"),
            '%' => out.push_str("%%"),
            _ => out.push(c),
        }
    }
    out.push('"');
    Some(out)
}

/// Valor de string comum (`Icon=`): só o `\` precisa de escape.
fn string_value(s: &str) -> Option<String> {
    (!s.chars().any(char::is_control)).then(|| s.replace('\\', r"\\"))
}

/// Grava em um arquivo temporário e renomeia: quem ler nunca vê meio arquivo.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
    fs::create_dir_all(dir)?;
    let name = path.file_name().ok_or(io::ErrorKind::InvalidInput)?;
    let mut tmp_name = OsString::from(".");
    tmp_name.push(name);
    tmp_name.push(format!(".{}.tmp", std::process::id()));
    let tmp = dir.join(tmp_name);
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

/// `$XDG_DATA_HOME`, ou `~/.local/share`. Caminhos relativos são inválidos.
fn data_home(xdg: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let absolute = |v: OsString| Some(PathBuf::from(v)).filter(|p| p.is_absolute());
    xdg.and_then(absolute)
        .or_else(|| home.and_then(absolute).map(|h| h.join(".local/share")))
}

/// `$XDG_DATA_DIRS`, ou o padrão `/usr/local/share:/usr/share`.
fn data_dirs(xdg: Option<OsString>) -> Vec<PathBuf> {
    let value = xdg
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    env::split_paths(&value)
        .filter(|p| p.is_absolute())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "rusted-serial";
    const PNG: &[u8] = b"\x89PNG fake";

    /// Pasta temporária própria de cada teste (apagada no fim).
    struct Tmp(PathBuf);
    impl Tmp {
        fn new(name: &str) -> Self {
            let dir = env::temp_dir().join(format!("rs-desktop-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn entry_path(home: &Path) -> PathBuf {
        home.join("applications/rusted-serial.desktop")
    }

    #[test]
    fn cria_arquivo_oculto_e_icone() {
        let t = Tmp::new("create");
        let home = t.0.join("home");
        let exe = Path::new("/opt/apps/Rusted Serial.AppImage");
        sync(ID, &home, &[t.0.join("sys")], Some(exe), PNG).unwrap();

        let text = fs::read_to_string(entry_path(&home)).unwrap();
        assert!(text.contains("Exec=\"/opt/apps/Rusted Serial.AppImage\"\n"));
        assert!(text.contains(&format!(
            "Icon={}\n",
            home.join(ID).join("rusted-serial.png").display()
        )));
        assert!(text.contains("NoDisplay=true\n"));
        assert!(is_ours(&text));
        assert_eq!(
            fs::read(home.join(ID).join("rusted-serial.png")).unwrap(),
            PNG
        );
    }

    #[test]
    fn atualiza_exec_quando_o_appimage_muda_de_pasta() {
        let t = Tmp::new("moved");
        let home = t.0.join("home");
        sync(ID, &home, &[], Some(Path::new("/a/app.AppImage")), PNG).unwrap();
        sync(ID, &home, &[], Some(Path::new("/b/app.AppImage")), PNG).unwrap();
        let text = fs::read_to_string(entry_path(&home)).unwrap();
        assert!(text.contains("Exec=\"/b/app.AppImage\""));
        assert!(!text.contains("/a/app.AppImage"));
    }

    #[test]
    fn nunca_mexe_no_arquivo_de_outro() {
        let t = Tmp::new("foreign");
        let home = t.0.join("home");
        fs::create_dir_all(home.join("applications")).unwrap();
        let theirs = "[Desktop Entry]\nName=Meu atalho\n";
        fs::write(entry_path(&home), theirs).unwrap();
        sync(ID, &home, &[], Some(Path::new("/x/app")), PNG).unwrap();
        assert_eq!(fs::read_to_string(entry_path(&home)).unwrap(), theirs);
    }

    #[test]
    fn pacote_instalado_vence_e_o_nosso_sai() {
        let t = Tmp::new("pkg");
        let (home, sys) = (t.0.join("home"), t.0.join("sys"));
        sync(
            ID,
            &home,
            std::slice::from_ref(&sys),
            Some(Path::new("/x/app")),
            PNG,
        )
        .unwrap();
        assert!(entry_path(&home).exists());

        // Um .rpm/.deb instalou o arquivo do sistema.
        fs::create_dir_all(sys.join("applications")).unwrap();
        fs::write(
            sys.join("applications/rusted-serial.desktop"),
            "[Desktop Entry]\n",
        )
        .unwrap();
        sync(ID, &home, &[sys], Some(Path::new("/x/app")), PNG).unwrap();
        assert!(!entry_path(&home).exists());
    }

    #[test]
    fn data_home_dentro_de_data_dirs_nao_conta_como_instalado() {
        let t = Tmp::new("loop");
        let home = t.0.join("home");
        for _ in 0..2 {
            sync(
                ID,
                &home,
                std::slice::from_ref(&home),
                Some(Path::new("/x/app")),
                PNG,
            )
            .unwrap();
            assert!(entry_path(&home).exists());
        }
    }

    #[test]
    fn aspas_do_exec_seguem_a_especificacao() {
        assert_eq!(exec_arg("/a b/c").unwrap(), "\"/a b/c\"");
        assert_eq!(exec_arg("/a\"b").unwrap(), r#""/a\\"b""#);
        assert_eq!(exec_arg("/$x`y").unwrap(), r#""/\\$x\\`y""#);
        assert_eq!(exec_arg(r"/a\b").unwrap(), r#""/a\\\\b""#);
        assert_eq!(exec_arg("/100%").unwrap(), "\"/100%%\"");
        assert_eq!(exec_arg("/a\nb"), None);
    }

    #[test]
    fn caminhos_xdg() {
        assert_eq!(
            data_home(Some("rel/dir".into()), Some("/home/u".into())),
            Some(PathBuf::from("/home/u/.local/share"))
        );
        assert_eq!(
            data_home(Some("/data".into()), Some("/home/u".into())),
            Some(PathBuf::from("/data"))
        );
        assert_eq!(data_home(None, None), None);
        assert_eq!(
            data_dirs(None),
            [
                PathBuf::from("/usr/local/share"),
                PathBuf::from("/usr/share")
            ]
        );
        assert_eq!(
            data_dirs(Some("/x:rel:/y".into())),
            [PathBuf::from("/x"), PathBuf::from("/y")]
        );
    }
}
