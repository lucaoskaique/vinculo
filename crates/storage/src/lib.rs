//! Persistência local do Vínculo (SQLite).
//!
//! v1 usa SQLite puro; a migração para SQLCipher (banco criptografado em
//! repouso) está planejada antes de guardar qualquer dado clínico real.

use std::path::Path;

use anyhow::Result;
use chrono::{Local, NaiveDate, NaiveDateTime};
use rusqlite::{params, Connection, OptionalExtension};
use vinculo_core::{Evolucao, Paciente, Sessao, StatusSessao};

const SCHEMA: &str = include_str!("schema.sql");

pub struct Db {
    conn: Connection,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    pub fn inserir_paciente(
        &self,
        nome: &str,
        telefone: Option<&str>,
        email: Option<&str>,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO pacientes (nome, telefone, email) VALUES (?1, ?2, ?3)",
            params![nome, telefone, email],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn listar_pacientes(&self) -> Result<Vec<Paciente>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, nome, telefone, email FROM pacientes ORDER BY nome")?;
        let rows = stmt.query_map([], |r| {
            Ok(Paciente {
                id: r.get(0)?,
                nome: r.get(1)?,
                telefone: r.get(2)?,
                email: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn inserir_sessao(
        &self,
        paciente_id: i64,
        inicio: NaiveDateTime,
        duracao_min: u32,
        valor_centavos: i64,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO sessoes (paciente_id, inicio, duracao_min, valor_centavos)
             VALUES (?1, ?2, ?3, ?4)",
            params![paciente_id, inicio, duracao_min, valor_centavos],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Sessões de um dia, com o nome do paciente, ordenadas por horário.
    pub fn sessoes_do_dia(&self, dia: NaiveDate) -> Result<Vec<(Sessao, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.id, s.paciente_id, s.inicio, s.duracao_min, s.status,
                    s.valor_centavos, s.pago, p.nome
             FROM sessoes s
             JOIN pacientes p ON p.id = s.paciente_id
             WHERE date(s.inicio) = ?1
             ORDER BY s.inicio",
        )?;
        let rows = stmt.query_map(params![dia.format("%Y-%m-%d").to_string()], |r| {
            Ok((
                Sessao {
                    id: r.get(0)?,
                    paciente_id: r.get(1)?,
                    inicio: r.get(2)?,
                    duracao_min: r.get(3)?,
                    status: StatusSessao::from_db(&r.get::<_, String>(4)?),
                    valor_centavos: r.get(5)?,
                    pago: r.get::<_, i64>(6)? != 0,
                },
                r.get::<_, String>(7)?,
            ))
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Uma sessão com o nome do paciente.
    pub fn sessao_com_nome(&self, id: i64) -> Result<(Sessao, String)> {
        let resultado = self.conn.query_row(
            "SELECT s.id, s.paciente_id, s.inicio, s.duracao_min, s.status,
                    s.valor_centavos, s.pago, p.nome
             FROM sessoes s
             JOIN pacientes p ON p.id = s.paciente_id
             WHERE s.id = ?1",
            params![id],
            |r| {
                Ok((
                    Sessao {
                        id: r.get(0)?,
                        paciente_id: r.get(1)?,
                        inicio: r.get(2)?,
                        duracao_min: r.get(3)?,
                        status: StatusSessao::from_db(&r.get::<_, String>(4)?),
                        valor_centavos: r.get(5)?,
                        pago: r.get::<_, i64>(6)? != 0,
                    },
                    r.get::<_, String>(7)?,
                ))
            },
        )?;
        Ok(resultado)
    }

    /// Sessões de um mês (para o financeiro), ordenadas por data.
    pub fn sessoes_do_mes(&self, ano: i32, mes: u32) -> Result<Vec<(Sessao, String)>> {
        let prefixo = format!("{:04}-{:02}", ano, mes);
        let mut stmt = self.conn.prepare(
            "SELECT s.id, s.paciente_id, s.inicio, s.duracao_min, s.status,
                    s.valor_centavos, s.pago, p.nome
             FROM sessoes s
             JOIN pacientes p ON p.id = s.paciente_id
             WHERE substr(s.inicio, 1, 7) = ?1
             ORDER BY s.inicio",
        )?;
        let rows = stmt.query_map(params![prefixo], |r| {
            Ok((
                Sessao {
                    id: r.get(0)?,
                    paciente_id: r.get(1)?,
                    inicio: r.get(2)?,
                    duracao_min: r.get(3)?,
                    status: StatusSessao::from_db(&r.get::<_, String>(4)?),
                    valor_centavos: r.get(5)?,
                    pago: r.get::<_, i64>(6)? != 0,
                },
                r.get::<_, String>(7)?,
            ))
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn alternar_pago(&self, sessao_id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE sessoes SET pago = 1 - pago WHERE id = ?1",
            params![sessao_id],
        )?;
        Ok(())
    }

    pub fn definir_status(&self, sessao_id: i64, status: StatusSessao) -> Result<()> {
        self.conn.execute(
            "UPDATE sessoes SET status = ?2 WHERE id = ?1",
            params![sessao_id, status.as_db()],
        )?;
        Ok(())
    }

    /// Registra (ou atualiza) o caminho do áudio da smart note da sessão.
    pub fn salvar_audio_nota(&self, sessao_id: i64, caminho: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO smart_notes (sessao_id, audio_path) VALUES (?1, ?2)
             ON CONFLICT(sessao_id) DO UPDATE SET audio_path = excluded.audio_path",
            params![sessao_id, caminho],
        )?;
        Ok(())
    }

    /// Registra (ou atualiza) a transcrição da smart note da sessão.
    pub fn salvar_transcricao(&self, sessao_id: i64, texto: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO smart_notes (sessao_id, transcricao) VALUES (?1, ?2)
             ON CONFLICT(sessao_id) DO UPDATE SET transcricao = excluded.transcricao",
            params![sessao_id, texto],
        )?;
        Ok(())
    }

    /// Remove a referência ao áudio da sessão (o arquivo em si é removido pelo app).
    pub fn remover_audio_nota(&self, sessao_id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE smart_notes SET audio_path = NULL WHERE sessao_id = ?1",
            params![sessao_id],
        )?;
        Ok(())
    }

    /// (caminho_do_audio, transcricao) da smart note da sessão, se existir.
    pub fn nota_da_sessao(
        &self,
        sessao_id: i64,
    ) -> Result<Option<(Option<String>, Option<String>)>> {
        let resultado = self
            .conn
            .query_row(
                "SELECT audio_path, transcricao FROM smart_notes WHERE sessao_id = ?1",
                params![sessao_id],
                |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?,
                        r.get::<_, Option<String>>(1)?,
                    ))
                },
            )
            .optional()?;
        Ok(resultado)
    }

    pub fn inserir_evolucao(
        &self,
        paciente_id: i64,
        sessao_id: Option<i64>,
        conteudo: &str,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO evolucoes (paciente_id, sessao_id, conteudo) VALUES (?1, ?2, ?3)",
            params![paciente_id, sessao_id, conteudo],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Evoluções de um paciente, da mais recente para a mais antiga.
    pub fn evolucoes_do_paciente(&self, paciente_id: i64) -> Result<Vec<Evolucao>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, paciente_id, sessao_id, conteudo, criado_em
             FROM evolucoes WHERE paciente_id = ?1 ORDER BY criado_em DESC, id DESC",
        )?;
        let rows = stmt.query_map(params![paciente_id], |r| {
            Ok(Evolucao {
                id: r.get(0)?,
                paciente_id: r.get(1)?,
                sessao_id: r.get(2)?,
                conteudo: r.get(3)?,
                criado_em: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Uma evolução com o nome do paciente (para exportação).
    pub fn evolucao_por_id(&self, id: i64) -> Result<(Evolucao, String)> {
        let resultado = self.conn.query_row(
            "SELECT e.id, e.paciente_id, e.sessao_id, e.conteudo, e.criado_em, p.nome
             FROM evolucoes e
             JOIN pacientes p ON p.id = e.paciente_id
             WHERE e.id = ?1",
            params![id],
            |r| {
                Ok((
                    Evolucao {
                        id: r.get(0)?,
                        paciente_id: r.get(1)?,
                        sessao_id: r.get(2)?,
                        conteudo: r.get(3)?,
                        criado_em: r.get(4)?,
                    },
                    r.get::<_, String>(5)?,
                ))
            },
        )?;
        Ok(resultado)
    }

    /// Grava (ou atualiza) um valor de configuração do app.
    pub fn definir_config(&self, chave: &str, valor: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO config (chave, valor) VALUES (?1, ?2)
             ON CONFLICT(chave) DO UPDATE SET valor = excluded.valor",
            params![chave, valor],
        )?;
        Ok(())
    }

    pub fn obter_config(&self, chave: &str) -> Result<Option<String>> {
        let valor = self
            .conn
            .query_row(
                "SELECT valor FROM config WHERE chave = ?1",
                params![chave],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        Ok(valor)
    }

    /// Popula dados de demonstração na primeira execução (banco vazio).
    pub fn semear_exemplo(&self) -> Result<()> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM pacientes", [], |r| r.get(0))?;
        if n > 0 {
            return Ok(());
        }

        let hoje = Local::now().date_naive();
        let exemplos = [
            ("Ana Souza", 9, true),
            ("Bruno Lima", 10, false),
            ("Carla Mendes", 14, false),
        ];
        for (nome, hora, pago) in exemplos {
            let id = self.inserir_paciente(nome, None, None)?;
            let inicio = hoje.and_hms_opt(hora, 0, 0).expect("hora válida");
            let sessao_id = self.inserir_sessao(id, inicio, 50, 20000)?;
            if pago {
                self.conn.execute(
                    "UPDATE sessoes SET pago = 1 WHERE id = ?1",
                    params![sessao_id],
                )?;
            }
        }
        Ok(())
    }
}
