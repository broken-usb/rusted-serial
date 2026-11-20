//! Script de build: no Windows, embute no `.exe` o ícone e os dados da versão
//! (nome, versão, descrição), que aparecem no Explorer e nas propriedades do arquivo.
//! Nos outros sistemas não faz nada.

fn main() -> std::io::Result<()> {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    windows::embed_resources()
}

#[cfg(windows)]
mod windows {
    pub fn embed_resources() -> std::io::Result<()> {
        // Versão, nome e descrição vêm do Cargo.toml automaticamente.
        winresource::WindowsResource::new()
            .set_icon("assets/icon.ico")
            .set("ProductName", "Rusted Serial")
            .set("FileDescription", "Rusted Serial — terminal serial")
            .compile()
    }
}

#[cfg(not(windows))]
mod windows {
    pub fn embed_resources() -> std::io::Result<()> {
        Ok(())
    }
}
