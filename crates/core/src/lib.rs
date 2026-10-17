//! Tipos de domínio do Vínculo. Sem dependência de UI ou de banco.

use std::path::PathBuf;

use chrono::NaiveDateTime;

/// Diretório de dados do app, por plataforma:
/// Windows `%APPDATA%\Vinculo`, macOS `~/Library/Application Support/Vinculo`,
/// Linux `~/.local/share/Vinculo`.
pub fn dir_dados() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Vinculo")
}

/// Pasta de documentos do usuário + "Vinculo" (destino dos PDFs exportados).
pub fn dir_documentos() -> PathBuf {
    dirs::document_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Vinculo")
}

#[derive(Debug, Clone)]
pub struct Paciente {
    pub id: i64,
    pub nome: String,
    pub telefone: Option<String>,
    pub email: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusSessao {
    Agendada,
    Realizada,
    Cancelada,
    Faltou,
}

impl StatusSessao {
    pub fn as_db(&self) -> &'static str {
        match self {
            StatusSessao::Agendada => "agendada",
            StatusSessao::Realizada => "realizada",
            StatusSessao::Cancelada => "cancelada",
            StatusSessao::Faltou => "faltou",
        }
    }

    pub fn from_db(s: &str) -> Self {
        match s {
            "realizada" => StatusSessao::Realizada,
            "cancelada" => StatusSessao::Cancelada,
            "faltou" => StatusSessao::Faltou,
            _ => StatusSessao::Agendada,
        }
    }

    pub fn rotulo(&self) -> &'static str {
        match self {
            StatusSessao::Agendada => "Agendada",
            StatusSessao::Realizada => "Realizada",
            StatusSessao::Cancelada => "Cancelada",
            StatusSessao::Faltou => "Faltou",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Evolucao {
    pub id: i64,
    pub paciente_id: i64,
    pub sessao_id: Option<i64>,
    pub conteudo: String,
    pub criado_em: String,
}

#[derive(Debug, Clone)]
pub struct Sessao {
    pub id: i64,
    pub paciente_id: i64,
    /// Hora local do consultório.
    pub inicio: NaiveDateTime,
    pub duracao_min: u32,
    pub status: StatusSessao,
    pub valor_centavos: i64,
    pub pago: bool,
}
