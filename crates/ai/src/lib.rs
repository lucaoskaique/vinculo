//! Anonimização e geração de rascunho de evolução clínica via Claude API.
//!
//! Fluxo: a transcrição da sessão é anonimizada localmente (nomes, e-mails,
//! telefones, CPFs), enviada à Claude API para gerar um RASCUNHO de evolução
//! clínica, e o resultado é desanonimizado. O rascunho é sempre revisado pelo
//! profissional antes de ser salvo no prontuário.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};

/// Configuração de acesso à Claude API.
pub struct ConfigIa {
    pub api_key: String,
}

impl ConfigIa {
    /// Carrega a chave de `ANTHROPIC_API_KEY` ou de `%APPDATA%\Vinculo\config.toml`
    /// (campo `api_key = "..."`).
    pub fn carregar() -> Result<ConfigIa> {
        // 1) Variável de ambiente tem prioridade.
        if let Ok(chave) = std::env::var("ANTHROPIC_API_KEY") {
            let chave = chave.trim().to_string();
            if !chave.is_empty() {
                return Ok(ConfigIa { api_key: chave });
            }
        }

        // 2) Arquivo de configuração no diretório de dados do app.
        let caminho = caminho_config()?;
        if caminho.exists() {
            let conteudo = std::fs::read_to_string(&caminho).with_context(|| {
                format!("não foi possível ler o arquivo de configuração: {}", caminho.display())
            })?;
            if let Some(chave) = extrair_api_key_toml(&conteudo) {
                return Ok(ConfigIa { api_key: chave });
            }
            anyhow::bail!(
                "o arquivo {} existe, mas não contém o campo api_key = \"...\"",
                caminho.display()
            );
        }

        anyhow::bail!(
            "Chave da API não configurada. Configure na tela Configurações do app, \
             ou defina a variável de ambiente ANTHROPIC_API_KEY, ou crie o arquivo {} \
             contendo a linha: api_key = \"sua-chave-aqui\"",
            caminho.display()
        )
    }
}

/// Extrai o valor de `api_key = "..."` de um TOML simples, sem dependência externa.
/// Ignora comentários e espaços; aceita aspas simples ou duplas.
fn extrair_api_key_toml(conteudo: &str) -> Option<String> {
    for linha in conteudo.lines() {
        let linha = linha.trim();
        if linha.starts_with('#') {
            continue;
        }
        if let Some((chave, valor)) = linha.split_once('=') {
            if chave.trim() == "api_key" {
                // Remove comentário no fim da linha, se houver, e as aspas.
                let valor = valor.trim();
                let valor = valor
                    .trim_start_matches(['"', '\''])
                    .split(['"', '\''])
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if !valor.is_empty() {
                    return Some(valor);
                }
            }
        }
    }
    None
}

fn caminho_config() -> Result<PathBuf> {
    Ok(vinculo_core::dir_dados().join("config.toml"))
}

/// Grava (ou substitui) o campo `api_key` no config.toml do diretório de dados
/// do app, preservando as demais linhas do arquivo. Retorna o caminho do arquivo.
pub fn salvar_api_key(chave: &str) -> Result<PathBuf> {
    let chave = chave.trim();
    if chave.is_empty() {
        anyhow::bail!("a chave não pode ser vazia");
    }
    let caminho = caminho_config()?;
    if let Some(pai) = caminho.parent() {
        std::fs::create_dir_all(pai)
            .with_context(|| format!("falha ao criar o diretório {}", pai.display()))?;
    }
    let existente = std::fs::read_to_string(&caminho).unwrap_or_default();
    let mut linhas: Vec<String> = existente
        .lines()
        .filter(|l| {
            l.trim()
                .split_once('=')
                .map(|(k, _)| k.trim() != "api_key")
                .unwrap_or(true)
        })
        .map(str::to_string)
        .collect();
    linhas.push(format!("api_key = \"{}\"", chave.replace('"', "")));
    std::fs::write(&caminho, linhas.join("\n") + "\n")
        .with_context(|| format!("falha ao gravar {}", caminho.display()))?;
    Ok(caminho)
}

/// De onde a chave da API seria carregada agora, em texto para exibição.
/// `None` = nenhuma chave configurada.
pub fn origem_api_key() -> Option<String> {
    if std::env::var("ANTHROPIC_API_KEY").map(|v| !v.trim().is_empty()).unwrap_or(false) {
        return Some("variável de ambiente ANTHROPIC_API_KEY".to_string());
    }
    let caminho = caminho_config().ok()?;
    let conteudo = std::fs::read_to_string(&caminho).ok()?;
    extrair_api_key_toml(&conteudo).map(|_| "arquivo config.toml".to_string())
}

/// Substitui nomes conhecidos e padrões identificadores (e-mail, telefone, CPF)
/// por placeholders. Retorna o texto anonimizado e o mapa (placeholder, original).
///
/// - Cada pessoa em `nomes` recebe um placeholder `[PESSOA_n]`; o nome completo
///   e cada parte do nome com 3+ letras são substituídos pelo mesmo placeholder
///   (busca sem distinção de maiúsculas/minúsculas).
/// - E-mails viram `[EMAIL_n]`, telefones brasileiros `[TELEFONE_n]` e
///   CPFs `[CPF_n]`. Uma entrada no mapa por placeholder distinto.
pub fn anonimizar(texto: &str, nomes: &[&str]) -> (String, Vec<(String, String)>) {
    let mut mapa: Vec<(String, String)> = Vec::new();
    let mut resultado = texto.to_string();

    // E-mails primeiro, para que partes de nomes dentro de endereços não sejam
    // substituídas separadamente (o endereço inteiro vira um único placeholder).
    let re_email = Regex::new(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}").unwrap();
    resultado = substituir_padrao(&resultado, &re_email, "EMAIL", &mut mapa);

    // CPF formatado (000.000.000-00) antes dos telefones, para evitar conflito.
    let re_cpf_fmt = Regex::new(r"\b\d{3}\.\d{3}\.\d{3}-\d{2}\b").unwrap();
    resultado = substituir_padrao(&resultado, &re_cpf_fmt, "CPF", &mut mapa);

    // Telefones brasileiros: +55 ..., (21) 99999-9999, 21 99999-9999,
    // 21999999999 (11 dígitos com nono dígito), 99999-9999.
    let re_telefone = Regex::new(
        r"\+55[\s.-]?\(?\d{2}\)?[\s.-]?9?\d{4}[\s.-]?\d{4}|\(\d{2}\)[\s.-]?9?\d{4}[\s.-]?\d{4}|\b\d{2}\s?9\d{4}-?\d{4}\b|\b9?\d{4}-\d{4}\b",
    )
    .unwrap();
    resultado = substituir_padrao(&resultado, &re_telefone, "TELEFONE", &mut mapa);

    // CPF em 11 dígitos corridos (o que sobrou após os telefones).
    let re_cpf_num = Regex::new(r"\b\d{11}\b").unwrap();
    resultado = substituir_padrao(&resultado, &re_cpf_num, "CPF", &mut mapa);

    // Nomes de pessoas: nome completo e cada parte com 3+ letras, mesmo placeholder.
    for (i, nome) in nomes.iter().enumerate() {
        let nome = nome.trim();
        if nome.is_empty() {
            continue;
        }
        let placeholder = format!("[PESSOA_{}]", i + 1);

        // Alternativas: nome completo primeiro (mais longo), depois as partes.
        let mut alternativas: Vec<String> = vec![regex::escape(nome)];
        let mut vistas: Vec<String> = vec![nome.to_lowercase()];
        for parte in nome.split_whitespace() {
            if parte.chars().count() >= 3 && !vistas.contains(&parte.to_lowercase()) {
                vistas.push(parte.to_lowercase());
                alternativas.push(regex::escape(parte));
            }
        }
        let padrao = format!(r"(?i)\b(?:{})\b", alternativas.join("|"));
        let re_nome = Regex::new(&padrao).expect("regex de nome inválida");

        if re_nome.is_match(&resultado) {
            resultado = re_nome.replace_all(&resultado, placeholder.as_str()).into_owned();
            // Uma entrada por pessoa: o placeholder restaura o nome completo.
            mapa.push((placeholder, nome.to_string()));
        }
    }

    (resultado, mapa)
}

/// Substitui todas as ocorrências de `re` por placeholders `[PREFIXO_n]`,
/// reutilizando o mesmo placeholder para ocorrências idênticas, e registra
/// uma entrada no mapa por placeholder distinto.
fn substituir_padrao(
    texto: &str,
    re: &Regex,
    prefixo: &str,
    mapa: &mut Vec<(String, String)>,
) -> String {
    let mut cache: HashMap<String, String> = HashMap::new();
    let mut contador = 0usize;
    re.replace_all(texto, |caps: &regex::Captures| {
        let original = caps[0].to_string();
        cache
            .entry(original.clone())
            .or_insert_with(|| {
                contador += 1;
                let placeholder = format!("[{}_{}]", prefixo, contador);
                mapa.push((placeholder.clone(), original));
                placeholder
            })
            .clone()
    })
    .into_owned()
}

/// Restaura os valores originais a partir do mapa (placeholder, original).
pub fn desanonimizar(texto: &str, mapa: &[(String, String)]) -> String {
    let mut resultado = texto.to_string();
    for (placeholder, original) in mapa {
        resultado = resultado.replace(placeholder, original);
    }
    resultado
}

// ---------------------------------------------------------------------------
// Claude API (HTTP direto via reqwest blocking — não há SDK Rust oficial)
// ---------------------------------------------------------------------------

const URL_API: &str = "https://api.anthropic.com/v1/messages";
const MODELO: &str = "claude-opus-4-8";

const PROMPT_SISTEMA: &str = "\
Você auxilia um(a) psicólogo(a) a redigir um RASCUNHO de anotação de sessão \
(evolução clínica de prontuário) a partir da transcrição anonimizada de uma \
sessão de terapia.

Regras obrigatórias:
- Os placeholders de anonimização (como [PESSOA_1], [EMAIL_1], [TELEFONE_1], [CPF_1]) \
devem ser mantidos intactos, exatamente como aparecem na transcrição. Não tente \
adivinhar ou substituir os dados por trás deles.
- Inicie com uma única linha avisando: este é um RASCUNHO gerado por IA que requer \
revisão e validação do(a) profissional responsável antes de ser salvo no prontuário.
- Estruture o documento EXATAMENTE neste formato markdown (ele será renderizado em PDF):

## Temas trabalhados

### <título curto do primeiro tema>
Resumo: <parágrafo descrevendo o que foi relatado e trabalhado sobre o tema>
Pontos registrados:
- <ponto objetivo registrado na sessão>
- <outro ponto, se houver>
Leitura pela abordagem: <parágrafo com a leitura clínica do padrão observado>

### <título do segundo tema>
(mesma estrutura; crie um bloco por tema relevante da sessão, tipicamente 2 a 6 temas)

## Intervenções e recursos utilizados
- <intervenção ou recurso utilizado pelo(a) profissional na sessão>
- <...>

## Resposta e evolução observada
<parágrafo(s) sobre como a pessoa atendida respondeu às intervenções e o que evoluiu>

## Combinados e próximos passos
- <combinado, tarefa ou foco para as próximas sessões>
- <...>

- Use tom técnico e sóbrio, adequado a um registro clínico; escreva de forma \
impessoal (\"foi relatado\", \"a pessoa atendida\").
- Baseie-se exclusivamente no conteúdo da transcrição: não invente fatos, \
diagnósticos, falas ou acontecimentos que não estejam presentes nela. Se uma \
seção não tiver informação suficiente, escreva \"Sem registros nesta sessão.\" nela.
- Não use negrito (**), tabelas nem outros elementos markdown além de ##, ### e listas com -.
- Escreva em português.";

#[derive(Serialize)]
struct RequisicaoMensagens<'a> {
    model: &'a str,
    max_tokens: u32,
    system: &'a str,
    messages: Vec<Mensagem<'a>>,
}

#[derive(Serialize)]
struct Mensagem<'a> {
    role: &'a str,
    content: &'a str,
}

/// Resposta da API de Messages (campos desconhecidos são ignorados pelo serde).
#[derive(Deserialize)]
struct RespostaMensagens {
    #[serde(default)]
    content: Vec<Bloco>,
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
struct Bloco {
    #[serde(rename = "type")]
    tipo: String,
    #[serde(default)]
    text: Option<String>,
}

/// Corpo de erro da API ({"error": {"type": ..., "message": ...}}).
#[derive(Deserialize)]
struct CorpoErro {
    error: Option<DetalheErro>,
}

#[derive(Deserialize)]
struct DetalheErro {
    message: Option<String>,
}

/// Gera um rascunho de evolução clínica a partir da transcrição anonimizada
/// (chamada bloqueante à Claude API).
pub fn gerar_rascunho(cfg: &ConfigIa, transcricao_anon: &str) -> Result<String> {
    let cliente = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(300)) // respostas longas podem demorar
        .build()
        .context("falha ao criar o cliente HTTP")?;

    let corpo = RequisicaoMensagens {
        model: MODELO,
        max_tokens: 8192,
        system: PROMPT_SISTEMA,
        messages: vec![Mensagem {
            role: "user",
            content: transcricao_anon,
        }],
    };

    let resposta = cliente
        .post(URL_API)
        .header("x-api-key", &cfg.api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&corpo)
        .send()
        .context("falha ao conectar à Claude API")?;

    let status = resposta.status();
    let texto_resposta = resposta
        .text()
        .context("falha ao ler o corpo da resposta da Claude API")?;

    if !status.is_success() {
        // Tenta extrair error.message do corpo; senão, inclui o corpo bruto.
        let detalhe = serde_json::from_str::<CorpoErro>(&texto_resposta)
            .ok()
            .and_then(|c| c.error)
            .and_then(|e| e.message)
            .unwrap_or_else(|| texto_resposta.clone());
        anyhow::bail!("erro da Claude API (HTTP {}): {}", status.as_u16(), detalhe);
    }

    let resposta: RespostaMensagens = serde_json::from_str(&texto_resposta)
        .context("falha ao interpretar a resposta JSON da Claude API")?;

    if resposta.stop_reason.as_deref() == Some("refusal") {
        anyhow::bail!(
            "o modelo recusou a solicitação por razões de segurança. \
             Revise o conteúdo da transcrição ou tente novamente."
        );
    }

    let texto: String = resposta
        .content
        .iter()
        .filter(|b| b.tipo == "text")
        .filter_map(|b| b.text.as_deref())
        .collect::<Vec<_>>()
        .join("");

    if texto.trim().is_empty() {
        anyhow::bail!("a Claude API retornou uma resposta sem conteúdo de texto");
    }

    Ok(texto)
}

// ---------------------------------------------------------------------------
// Testes
// ---------------------------------------------------------------------------

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn anonimiza_nome_composto_e_partes() {
        let texto = "Maria da Silva relatou ansiedade. Mais tarde, silva mencionou \
                     o trabalho, e maria falou da família.";
        let (anon, mapa) = anonimizar(texto, &["Maria da Silva"]);

        // Nome completo e partes (case-insensitive) usam o mesmo placeholder.
        assert!(anon.contains("[PESSOA_1] relatou ansiedade"));
        assert!(anon.contains("[PESSOA_1] mencionou"));
        assert!(anon.contains("[PESSOA_1] falou"));
        assert!(!anon.to_lowercase().contains("maria"));
        assert!(!anon.to_lowercase().contains("silva"));
        // Partícula "da" (menos de 3 letras) permanece no texto.
        assert!(anon.contains("falou da família"));
        // Uma única entrada no mapa para a pessoa.
        assert_eq!(
            mapa,
            vec![("[PESSOA_1]".to_string(), "Maria da Silva".to_string())]
        );
    }

    #[test]
    fn anonimiza_email() {
        let texto = "Contato: joao.pereira@exemplo.com.br para remarcar.";
        let (anon, mapa) = anonimizar(texto, &[]);
        assert_eq!(anon, "Contato: [EMAIL_1] para remarcar.");
        assert_eq!(
            mapa,
            vec![(
                "[EMAIL_1]".to_string(),
                "joao.pereira@exemplo.com.br".to_string()
            )]
        );
    }

    #[test]
    fn anonimiza_telefone_e_cpf() {
        let texto = "Telefones: (21) 99999-9999 e 21988887777. CPF: 123.456.789-00.";
        let (anon, _mapa) = anonimizar(texto, &[]);
        assert!(anon.contains("[TELEFONE_1]"));
        assert!(anon.contains("[TELEFONE_2]"));
        assert!(anon.contains("[CPF_1]"));
        assert!(!anon.contains("99999"));
        assert!(!anon.contains("123.456.789-00"));
    }

    #[test]
    fn ida_e_volta_anonimizar_desanonimizar() {
        let texto = "Ana Souza (ana.souza@mail.com, tel (11) 98765-4321) relatou \
                     melhora. Ana retomará na próxima semana.";
        let (anon, mapa) = anonimizar(texto, &["Ana Souza"]);

        // O texto anonimizado não contém os identificadores originais.
        assert!(!anon.contains("Ana"));
        assert!(!anon.contains("ana.souza@mail.com"));
        assert!(!anon.contains("98765"));

        // Desanonimizar restaura o texto original.
        // Observação: partes do nome são restauradas como o nome completo,
        // então usamos "Ana Souza" nas duas ocorrências para o teste exato.
        let texto_completo = "Ana Souza (ana.souza@mail.com, tel (11) 98765-4321) relatou \
                              melhora. Ana Souza retomará na próxima semana.";
        let (anon2, mapa2) = anonimizar(texto_completo, &["Ana Souza"]);
        assert_eq!(desanonimizar(&anon2, &mapa2), texto_completo);

        // E o mapa do primeiro caso também restaura os padrões regex.
        let restaurado = desanonimizar(&anon, &mapa);
        assert!(restaurado.contains("ana.souza@mail.com"));
        assert!(restaurado.contains("(11) 98765-4321"));
    }

    #[test]
    fn extrai_api_key_do_toml() {
        let toml = "# configuração\napi_key = \"sk-ant-teste-123\"\noutra = 1\n";
        assert_eq!(
            extrair_api_key_toml(toml),
            Some("sk-ant-teste-123".to_string())
        );
        assert_eq!(extrair_api_key_toml("sem_chave = true"), None);
    }
}
