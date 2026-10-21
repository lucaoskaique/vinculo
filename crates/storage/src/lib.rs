//! Persistência local do Vínculo (SQLite + SQLCipher).
//!
//! O banco de produção é criptografado em repouso via SQLCipher
//! ([`Db::abrir_cifrado`]); a chave é derivada da senha do profissional pelo
//! próprio SQLCipher (PBKDF2). [`Db::open`] abre sem criptografia e existe
//! para testes e para detectar/migrar bancos legados em texto plano.

use std::path::Path;

use anyhow::{anyhow, Context, Result};
use chrono::{Local, NaiveDate, NaiveDateTime};
use rusqlite::{params, Connection, OptionalExtension};
use vinculo_core::{Evolucao, Paciente, Sessao, StatusSessao};

const SCHEMA: &str = include_str!("schema.sql");

pub struct Db {
    conn: Connection,
}

impl Db {
    /// Abre sem criptografia (testes e bancos legados).
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

    /// Abre (ou cria) o banco criptografado com a senha do profissional.
    /// Senha incorreta resulta em `Err`.
    pub fn abrir_cifrado(path: &Path, senha: &str) -> Result<Self> {
        if senha.is_empty() {
            anyhow::bail!("a senha não pode ser vazia");
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "key", senha)
            .context("falha ao aplicar a chave de criptografia")?;
        // Com a chave errada, a primeira leitura falha ("file is not a database").
        let valido: std::result::Result<i64, _> =
            conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0));
        if valido.is_err() {
            anyhow::bail!("Senha incorreta.");
        }
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    /// `true` quando o arquivo existe e abre sem senha (banco legado em texto plano).
    pub fn banco_em_texto_plano(path: &Path) -> bool {
        if !path.exists() {
            return false;
        }
        Connection::open(path)
            .and_then(|c| {
                c.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
                    r.get::<_, i64>(0)
                })
            })
            .is_ok()
    }

    /// Migra um banco em texto plano para o formato criptografado com `senha`,
    /// substituindo o arquivo original. O conteúdo é exportado para um arquivo
    /// temporário cifrado, validado, e só então colocado no lugar do antigo —
    /// que é removido (inclusive os arquivos -wal/-shm).
    pub fn cifrar_banco_existente(path: &Path, senha: &str) -> Result<()> {
        if senha.is_empty() {
            anyhow::bail!("a senha não pode ser vazia");
        }
        let temporario = path.with_extension("db.cifrando");
        let _ = std::fs::remove_file(&temporario);

        {
            let conn = Connection::open(path).context("falha ao abrir o banco atual")?;
            // Consolida o WAL no arquivo principal antes de exportar.
            let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_r| {
                Ok::<_, rusqlite::Error>(())
            });
            conn.execute(
                "ATTACH DATABASE ?1 AS cifrado KEY ?2",
                params![temporario.to_string_lossy(), senha],
            )
            .context("falha ao criar o banco cifrado temporário")?;
            conn.query_row("SELECT sqlcipher_export('cifrado')", [], |_r| {
                Ok::<_, rusqlite::Error>(())
            })
            .context("falha ao exportar os dados para o banco cifrado")?;
            conn.execute("DETACH DATABASE cifrado", [])?;
        }

        // Valida o arquivo cifrado antes de descartar o original.
        Db::abrir_cifrado(&temporario, senha)
            .map_err(|e| anyhow!("validação do banco cifrado falhou: {e}"))?;

        std::fs::remove_file(path).context("falha ao remover o banco antigo")?;
        std::fs::rename(&temporario, path).context("falha ao ativar o banco cifrado")?;
        for sufixo in ["db-wal", "db-shm"] {
            let _ = std::fs::remove_file(path.with_extension(sufixo));
        }
        Ok(())
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

    pub fn atualizar_paciente(
        &self,
        id: i64,
        nome: &str,
        telefone: Option<&str>,
        email: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE pacientes SET nome = ?2, telefone = ?3, email = ?4 WHERE id = ?1",
            params![id, nome, telefone, email],
        )?;
        Ok(())
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

#[cfg(test)]
mod testes {
    use super::*;

    fn caminho_tmp(nome: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("vinculo-testes-cripto");
        let _ = std::fs::create_dir_all(&dir);
        let caminho = dir.join(nome);
        let _ = std::fs::remove_file(&caminho);
        for sufixo in ["db-wal", "db-shm"] {
            let _ = std::fs::remove_file(caminho.with_extension(sufixo));
        }
        caminho
    }

    #[test]
    fn cifra_banco_legado_e_exige_senha() {
        let caminho = caminho_tmp("migracao.db");

        // Banco legado em texto plano com um paciente.
        {
            let db = Db::open(&caminho).unwrap();
            db.inserir_paciente("Teste da Silva", None, None).unwrap();
        }
        assert!(Db::banco_em_texto_plano(&caminho));

        // Migra para cifrado.
        Db::cifrar_banco_existente(&caminho, "senha-forte").unwrap();
        assert!(!Db::banco_em_texto_plano(&caminho));

        // Senha errada falha; senha certa lê os dados migrados.
        assert!(Db::abrir_cifrado(&caminho, "senha-errada").is_err());
        let db = Db::abrir_cifrado(&caminho, "senha-forte").unwrap();
        let pacientes = db.listar_pacientes().unwrap();
        assert_eq!(pacientes.len(), 1);
        assert_eq!(pacientes[0].nome, "Teste da Silva");
    }

    #[test]
    fn banco_novo_cifrado() {
        let caminho = caminho_tmp("novo.db");
        {
            let db = Db::abrir_cifrado(&caminho, "outra-senha").unwrap();
            db.inserir_paciente("Nova Pessoa", None, None).unwrap();
        }
        assert!(!Db::banco_em_texto_plano(&caminho));
        let db = Db::abrir_cifrado(&caminho, "outra-senha").unwrap();
        assert_eq!(db.listar_pacientes().unwrap().len(), 1);
    }
}
