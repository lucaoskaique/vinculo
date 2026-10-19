#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use anyhow::Result;
use chrono::{Datelike, Local, NaiveDate};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use vinculo_core::StatusSessao;
use vinculo_storage::Db;

slint::include_modules!();

fn dir_dados() -> PathBuf {
    vinculo_core::dir_dados()
}

const URL_MODELO: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin";

fn caminho_modelo() -> PathBuf {
    dir_dados().join("models").join("ggml-base.bin")
}

/// Baixa o modelo de transcrição para `destino`, reportando o progresso (0–100).
/// Grava num arquivo .part e renomeia ao final, para nunca deixar um modelo
/// corrompido no caminho definitivo.
fn baixar_modelo(destino: &std::path::Path, progresso: impl Fn(u32)) -> Result<()> {
    use std::io::{Read, Write};

    if let Some(pai) = destino.parent() {
        std::fs::create_dir_all(pai)?;
    }
    let cliente = reqwest::blocking::Client::builder()
        .timeout(None) // download longo; sem timeout global
        .build()?;
    let mut resposta = cliente.get(URL_MODELO).send()?.error_for_status()?;
    let total = resposta.content_length().unwrap_or(0);

    let temporario = destino.with_extension("part");
    let mut arquivo = std::fs::File::create(&temporario)?;
    let mut buffer = [0u8; 64 * 1024];
    let mut lido: u64 = 0;
    let mut ultimo_pct = 0u32;
    loop {
        let n = resposta.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        arquivo.write_all(&buffer[..n])?;
        lido += n as u64;
        if let Some(pct) = (lido * 100).checked_div(total) {
            let pct = pct as u32;
            if pct != ultimo_pct {
                ultimo_pct = pct;
                progresso(pct);
            }
        }
    }
    arquivo.flush()?;
    drop(arquivo);
    std::fs::rename(&temporario, destino)?;
    Ok(())
}

struct Estado {
    dia: NaiveDate,
    ano: i32,
    mes: u32,
    paciente_ids: Vec<i64>,
    sessao_aberta: Option<i64>,
    paciente_da_sessao: Option<i64>,
    paciente_em_edicao: Option<i64>,
    gravador: Option<vinculo_audio::Gravador>,
    reprodutor: Option<vinculo_audio::Reprodutor>,
    duracao_audio_s: f64,
    mapa_anon: Vec<(String, String)>,
    // Nomes reais dos dispositivos; o índice 0 das ComboBoxes é "Padrão do Windows".
    dispositivos_entrada: Vec<String>,
    dispositivos_saida: Vec<String>,
}

/// "200", "200,50" ou "1.200,50" → centavos.
fn para_centavos(texto: &str) -> i64 {
    let t = texto
        .trim()
        .replace("R$", "")
        .replace('.', "")
        .replace(',', ".");
    (t.trim().parse::<f64>().unwrap_or(0.0) * 100.0).round() as i64
}

fn em_reais(centavos: i64) -> String {
    format!("R$ {},{:02}", centavos / 100, (centavos % 100).abs())
}

fn recarregar_agenda(ui: &MainWindow, db: &Db, dia: NaiveDate) {
    let itens: Vec<SessaoVm> = db
        .sessoes_do_dia(dia)
        .unwrap_or_default()
        .into_iter()
        .map(|(s, nome)| SessaoVm {
            id: s.id as i32,
            hora: s.inicio.format("%H:%M").to_string().into(),
            paciente: nome.into(),
            status: s.status.rotulo().into(),
            pago: s.pago,
        })
        .collect();
    ui.set_sessoes(ModelRc::new(VecModel::from(itens)));
    ui.set_data_hoje(dia.format("%d/%m/%Y").to_string().into());
}

fn recarregar_pacientes(ui: &MainWindow, db: &Db) -> Vec<i64> {
    let pacientes = db.listar_pacientes().unwrap_or_default();
    let ids: Vec<i64> = pacientes.iter().map(|p| p.id).collect();
    let nomes: Vec<SharedString> = pacientes
        .iter()
        .map(|p| SharedString::from(p.nome.as_str()))
        .collect();
    let vms: Vec<PacienteVm> = pacientes
        .into_iter()
        .map(|p| PacienteVm {
            nome: p.nome.into(),
            telefone: p.telefone.unwrap_or_default().into(),
            email: p.email.unwrap_or_default().into(),
        })
        .collect();
    ui.set_pacientes(ModelRc::new(VecModel::from(vms)));
    ui.set_nomes_pacientes(ModelRc::new(VecModel::from(nomes)));
    ids
}

fn recarregar_financeiro(ui: &MainWindow, db: &Db, ano: i32, mes: u32) {
    let itens = db.sessoes_do_mes(ano, mes).unwrap_or_default();
    let mut recebido = 0i64;
    let mut pendente = 0i64;
    let vms: Vec<FinItemVm> = itens
        .into_iter()
        .map(|(s, nome)| {
            if s.pago {
                recebido += s.valor_centavos;
            } else {
                pendente += s.valor_centavos;
            }
            FinItemVm {
                id: s.id as i32,
                data: s.inicio.format("%d/%m %H:%M").to_string().into(),
                paciente: nome.into(),
                valor: em_reais(s.valor_centavos).into(),
                pago: s.pago,
            }
        })
        .collect();
    ui.set_fin_itens(ModelRc::new(VecModel::from(vms)));
    ui.set_fin_mes(format!("{:02}/{}", mes, ano).into());
    ui.set_fin_recebido(em_reais(recebido).into());
    ui.set_fin_pendente(em_reais(pendente).into());
}

/// "2026-10-17 15:04:00" (sqlite) → "17/10/2026 às 15:04"; devolve o original se não parsear.
fn formatar_data_registro(bruto: &str) -> SharedString {
    chrono::NaiveDateTime::parse_from_str(bruto, "%Y-%m-%d %H:%M:%S")
        .map(|dt| dt.format("%d/%m/%Y às %H:%M").to_string())
        .unwrap_or_else(|_| bruto.to_string())
        .into()
}

fn nome_arquivo_seguro(texto: &str) -> String {
    texto
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Gera o PDF da anotação em Documentos\Vinculo e abre no visualizador padrão.
fn exportar_pdf(db: &Db, paciente: &str, data_sessao: &str, conteudo: &str) -> Result<PathBuf> {
    let profissional = db
        .obter_config("perfil.nome")
        .ok()
        .flatten()
        .unwrap_or_default();
    let crp = db
        .obter_config("perfil.crp")
        .ok()
        .flatten()
        .unwrap_or_default();
    let destino_dir = vinculo_core::dir_documentos();
    std::fs::create_dir_all(&destino_dir)?;
    let destino = destino_dir.join(format!(
        "Anotacao_{}_{}.pdf",
        nome_arquivo_seguro(paciente),
        nome_arquivo_seguro(data_sessao)
    ));
    let dados = vinculo_relatorio::DadosAnotacao {
        paciente,
        data_sessao,
        profissional: &profissional,
        crp: &crp,
        conteudo_markdown: conteudo,
    };
    vinculo_relatorio::gerar_pdf_anotacao(&dados, &destino)?;
    let _ = open::that_detached(&destino); // abre no visualizador padrão da plataforma
    Ok(destino)
}

fn formatar_duracao(segundos: f64) -> String {
    let total = segundos.round() as u64;
    format!("{:02}:{:02}", total / 60, total % 60)
}

/// Atualiza na UI a existência, duração e forma de onda do áudio da sessão.
/// Retorna a duração em segundos (None quando não há áudio).
fn atualizar_info_audio(ui: &MainWindow, db: &Db, sessao_id: i64) -> Option<f64> {
    ui.set_progresso_audio(0.0);
    ui.set_confirmar_apagar(false);
    let caminho = db
        .nota_da_sessao(sessao_id)
        .ok()
        .flatten()
        .and_then(|(audio, _)| audio)
        .map(PathBuf::from)
        .filter(|p| p.exists());
    match caminho {
        Some(p) => {
            ui.set_tem_audio(true);
            let duracao = vinculo_audio::duracao_wav(&p).ok();
            ui.set_duracao_audio(duracao.map(formatar_duracao).unwrap_or_default().into());
            let picos = vinculo_audio::forma_de_onda(&p, 240).unwrap_or_default();
            ui.set_forma_onda(ModelRc::new(VecModel::from(picos)));
            duracao
        }
        None => {
            ui.set_tem_audio(false);
            ui.set_duracao_audio("".into());
            ui.set_forma_onda(ModelRc::new(VecModel::from(Vec::<f32>::new())));
            None
        }
    }
}

/// Máscaras de digitação dos campos de formulário.
fn aplicar_mascara(tipo: &str, texto: &str) -> String {
    let digitos: String = texto.chars().filter(|c| c.is_ascii_digit()).collect();
    match tipo {
        // HH:MM — insere o ":" após as duas primeiras casas.
        "hora" => {
            let d = &digitos[..digitos.len().min(4)];
            if d.len() <= 2 {
                d.to_string()
            } else {
                format!("{}:{}", &d[..2], &d[2..])
            }
        }
        // (DD) 99999-9999 — formata conforme digita, fixo e celular.
        "telefone" => {
            let d = &digitos[..digitos.len().min(11)];
            match d.len() {
                0 => String::new(),
                1..=2 => format!("({d}"),
                3..=6 => format!("({}) {}", &d[..2], &d[2..]),
                7..=10 => format!("({}) {}-{}", &d[..2], &d[2..d.len() - 4], &d[d.len() - 4..]),
                _ => format!("({}) {}-{}", &d[..2], &d[2..7], &d[7..]),
            }
        }
        // Valor em reais: dígitos e uma única vírgula com até 2 casas.
        "valor" => {
            let mut saida = String::new();
            let mut tem_virgula = false;
            let mut casas = 0;
            for c in texto.chars() {
                if c.is_ascii_digit() {
                    if tem_virgula {
                        if casas < 2 {
                            saida.push(c);
                            casas += 1;
                        }
                    } else {
                        saida.push(c);
                    }
                } else if (c == ',' || c == '.') && !tem_virgula && !saida.is_empty() {
                    saida.push(',');
                    tem_virgula = true;
                }
            }
            saida
        }
        // E-mail: sem espaços e em minúsculas.
        "email" => texto
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .to_lowercase(),
        _ => texto.to_string(),
    }
}

fn status_chave_ia() -> String {
    match vinculo_ai::origem_api_key() {
        Some(origem) => format!("✓ Chave configurada ({origem})."),
        None => "Nenhuma chave configurada — cole abaixo para habilitar a geração de rascunhos."
            .to_string(),
    }
}

fn recarregar_config(ui: &MainWindow, db: &Db) {
    let valor = |chave: &str| db.obter_config(chave).ok().flatten().unwrap_or_default();
    ui.set_cfg_nome(valor("perfil.nome").into());
    ui.set_cfg_crp(valor("perfil.crp").into());
    ui.set_cfg_email(valor("perfil.email").into());
    ui.set_cfg_telefone(valor("perfil.telefone").into());
    ui.set_cfg_workspace(vinculo_ai::obter_workspace_id().unwrap_or_default().into());
    ui.set_cfg_status_ia(status_chave_ia().into());
    ui.set_cfg_status("".into());
}

fn recarregar_prontuario(ui: &MainWindow, db: &Db, paciente_id: i64) {
    let vms: Vec<EvolucaoVm> = db
        .evolucoes_do_paciente(paciente_id)
        .unwrap_or_default()
        .into_iter()
        .map(|e| EvolucaoVm {
            id: e.id as i32,
            data: formatar_data_registro(&e.criado_em),
            conteudo: e.conteudo.into(),
        })
        .collect();
    ui.set_evolucoes(ModelRc::new(VecModel::from(vms)));
}

fn main() -> Result<()> {
    let ui = MainWindow::new()?;
    ui.window().set_maximized(true);
    let caminho_db = dir_dados().join("vinculo.db");
    let em_texto_plano = Db::banco_em_texto_plano(&caminho_db);
    let modo_criacao = !caminho_db.exists() || em_texto_plano;
    ui.set_trava_migracao(em_texto_plano);
    ui.set_trava_modo_criacao(modo_criacao);

    ui.on_desbloquear({
        let ui_weak = ui.as_weak();
        move |senha, confirmacao| {
            let ui = ui_weak.unwrap();
            let senha = senha.to_string();
            if modo_criacao {
                if senha.chars().count() < 6 {
                    ui.set_trava_status("A senha precisa ter pelo menos 6 caracteres.".into());
                    return;
                }
                if senha != confirmacao.as_str() {
                    ui.set_trava_status("As senhas não conferem.".into());
                    return;
                }
                if em_texto_plano {
                    ui.set_trava_status("Criptografando o banco existente…".into());
                    if let Err(e) = Db::cifrar_banco_existente(&caminho_db, &senha) {
                        ui.set_trava_status(format!("Falha ao criptografar: {e}").into());
                        return;
                    }
                }
            }
            match Db::abrir_cifrado(&caminho_db, &senha) {
                Ok(db) => {
                    let db = Rc::new(db);
                    let _ = db.semear_exemplo();
                    configurar_app(&ui, db);
                    ui.set_trava_senha("".into());
                    ui.set_trava_confirma("".into());
                    ui.set_trava_status("".into());
                    ui.set_tela(0);
                }
                Err(e) => ui.set_trava_status(format!("{e}").into()),
            }
        }
    });

    ui.run()?;
    Ok(())
}

/// Registra todos os callbacks e carrega os dados iniciais.
/// Só é chamado após o desbloqueio do banco criptografado.
fn configurar_app(ui: &MainWindow, db: Rc<Db>) {
    let hoje = Local::now().date_naive();
    let estado = Rc::new(RefCell::new(Estado {
        dia: hoje,
        ano: hoje.year(),
        mes: hoje.month(),
        paciente_ids: Vec::new(),
        sessao_aberta: None,
        paciente_da_sessao: None,
        paciente_em_edicao: None,
        gravador: None,
        reprodutor: None,
        duracao_audio_s: 0.0,
        mapa_anon: Vec::new(),
        dispositivos_entrada: Vec::new(),
        dispositivos_saida: Vec::new(),
    }));

    recarregar_agenda(&ui, &db, hoje);
    estado.borrow_mut().paciente_ids = recarregar_pacientes(&ui, &db);
    recarregar_financeiro(&ui, &db, hoje.year(), hoje.month());

    // ---------- navegação ----------
    ui.on_tela_mudou({
        let ui_weak = ui.as_weak();
        let db = Rc::clone(&db);
        let estado = Rc::clone(&estado);
        move |tela| {
            let ui = ui_weak.unwrap();
            match tela {
                0 => {
                    let dia = estado.borrow().dia;
                    recarregar_agenda(&ui, &db, dia);
                }
                1 | 2 => {
                    estado.borrow_mut().paciente_ids = recarregar_pacientes(&ui, &db);
                }
                3 => {
                    let (ano, mes) = {
                        let e = estado.borrow();
                        (e.ano, e.mes)
                    };
                    recarregar_financeiro(&ui, &db, ano, mes);
                }
                5 => recarregar_config(&ui, &db),
                _ => {}
            }
        }
    });

    ui.on_mudar_dia({
        let ui_weak = ui.as_weak();
        let db = Rc::clone(&db);
        let estado = Rc::clone(&estado);
        move |delta| {
            let ui = ui_weak.unwrap();
            let novo = {
                let mut e = estado.borrow_mut();
                e.dia += chrono::Duration::days(delta as i64);
                e.dia
            };
            recarregar_agenda(&ui, &db, novo);
        }
    });

    ui.on_mudar_mes({
        let ui_weak = ui.as_weak();
        let db = Rc::clone(&db);
        let estado = Rc::clone(&estado);
        move |delta| {
            let ui = ui_weak.unwrap();
            let (ano, mes) = {
                let mut e = estado.borrow_mut();
                let total = e.ano * 12 + e.mes as i32 - 1 + delta;
                e.ano = total.div_euclid(12);
                e.mes = (total.rem_euclid(12) + 1) as u32;
                (e.ano, e.mes)
            };
            recarregar_financeiro(&ui, &db, ano, mes);
        }
    });

    // ---------- pacientes (criar/editar) ----------
    ui.on_salvar_paciente({
        let ui_weak = ui.as_weak();
        let db = Rc::clone(&db);
        let estado = Rc::clone(&estado);
        move || {
            let ui = ui_weak.unwrap();
            let nome = ui.get_pac_nome().trim().to_string();
            if nome.is_empty() {
                return;
            }
            let tel = aplicar_mascara("telefone", ui.get_pac_telefone().as_str());
            let mail = aplicar_mascara("email", ui.get_pac_email().as_str());
            let resultado = match estado.borrow().paciente_em_edicao {
                Some(id) => db.atualizar_paciente(
                    id,
                    &nome,
                    (!tel.is_empty()).then_some(tel.as_str()),
                    (!mail.is_empty()).then_some(mail.as_str()),
                ),
                None => db
                    .inserir_paciente(
                        &nome,
                        (!tel.is_empty()).then_some(tel.as_str()),
                        (!mail.is_empty()).then_some(mail.as_str()),
                    )
                    .map(|_| ()),
            };
            if resultado.is_ok() {
                let mut e = estado.borrow_mut();
                e.paciente_em_edicao = None;
                e.paciente_ids = recarregar_pacientes(&ui, &db);
                ui.set_pac_nome("".into());
                ui.set_pac_telefone("".into());
                ui.set_pac_email("".into());
                ui.set_editando_paciente(false);
            }
        }
    });

    ui.on_editar_paciente({
        let ui_weak = ui.as_weak();
        let db = Rc::clone(&db);
        let estado = Rc::clone(&estado);
        move |indice| {
            let ui = ui_weak.unwrap();
            let id = {
                let e = estado.borrow();
                e.paciente_ids.get(indice.max(0) as usize).copied()
            };
            let Some(id) = id else { return };
            let Some(paciente) = db
                .listar_pacientes()
                .unwrap_or_default()
                .into_iter()
                .find(|p| p.id == id)
            else {
                return;
            };
            estado.borrow_mut().paciente_em_edicao = Some(id);
            ui.set_pac_nome(paciente.nome.into());
            ui.set_pac_telefone(paciente.telefone.unwrap_or_default().into());
            ui.set_pac_email(paciente.email.unwrap_or_default().into());
            ui.set_editando_paciente(true);
        }
    });

    ui.on_cancelar_edicao({
        let ui_weak = ui.as_weak();
        let estado = Rc::clone(&estado);
        move || {
            let ui = ui_weak.unwrap();
            estado.borrow_mut().paciente_em_edicao = None;
            ui.set_pac_nome("".into());
            ui.set_pac_telefone("".into());
            ui.set_pac_email("".into());
            ui.set_editando_paciente(false);
        }
    });

    // ---------- agenda ----------
    ui.on_criar_sessao({
        let ui_weak = ui.as_weak();
        let db = Rc::clone(&db);
        let estado = Rc::clone(&estado);
        move |indice, hora, valor| {
            let ui = ui_weak.unwrap();
            let (dia, paciente_id) = {
                let e = estado.borrow();
                let Some(&id) = e.paciente_ids.get(indice.max(0) as usize) else {
                    return;
                };
                (e.dia, id)
            };
            let partes: Vec<&str> = hora.trim().split(':').collect();
            let (Some(h), Some(m)) = (
                partes.first().and_then(|p| p.parse::<u32>().ok()),
                partes.get(1).and_then(|p| p.parse::<u32>().ok()),
            ) else {
                return;
            };
            let Some(inicio) = dia.and_hms_opt(h, m, 0) else {
                return;
            };
            let _ = db.inserir_sessao(paciente_id, inicio, 50, para_centavos(&valor));
            recarregar_agenda(&ui, &db, dia);
        }
    });

    ui.on_abrir_sessao({
        let ui_weak = ui.as_weak();
        let db = Rc::clone(&db);
        let estado = Rc::clone(&estado);
        move |id| {
            let ui = ui_weak.unwrap();
            let id = id as i64;
            let Ok((sessao, nome)) = db.sessao_com_nome(id) else {
                return;
            };
            {
                let mut e = estado.borrow_mut();
                e.sessao_aberta = Some(id);
                e.paciente_da_sessao = Some(sessao.paciente_id);
            }
            ui.set_sessao_titulo(
                format!("{} — {}", nome, sessao.inicio.format("%d/%m/%Y %H:%M")).into(),
            );
            let transcricao = db
                .nota_da_sessao(id)
                .ok()
                .flatten()
                .and_then(|(_, t)| t)
                .unwrap_or_default();
            ui.set_transcricao(transcricao.into());
            ui.set_rascunho("".into());
            ui.set_gravando(false);
            ui.set_ocupado(false);
            // Interrompe qualquer reprodução anterior e atualiza o player.
            if let Some(reprodutor) = estado.borrow_mut().reprodutor.take() {
                reprodutor.parar();
            }
            ui.set_reproduzindo(false);
            let duracao = atualizar_info_audio(&ui, &db, id);
            estado.borrow_mut().duracao_audio_s = duracao.unwrap_or(0.0);
            ui.set_precisa_modelo(!caminho_modelo().exists());
            // Dispositivos de áudio disponíveis (índice 0 = padrão do Windows).
            let entradas = vinculo_audio::listar_entradas();
            let saidas = vinculo_audio::listar_saidas();
            let com_padrao = |lista: &[String]| -> Vec<SharedString> {
                std::iter::once(SharedString::from("Padrão do Windows"))
                    .chain(lista.iter().map(|n| SharedString::from(n.as_str())))
                    .collect()
            };
            ui.set_microfones(ModelRc::new(VecModel::from(com_padrao(&entradas))));
            ui.set_saidas_audio(ModelRc::new(VecModel::from(com_padrao(&saidas))));
            {
                let mut e = estado.borrow_mut();
                e.dispositivos_entrada = entradas;
                e.dispositivos_saida = saidas;
            }
            ui.set_status_nota(
                "Grave apenas com consentimento explícito do paciente. O áudio e a transcrição ficam nesta máquina."
                    .into(),
            );
            ui.set_tela(4);
        }
    });

    ui.on_marcar_realizada({
        let ui_weak = ui.as_weak();
        let db = Rc::clone(&db);
        let estado = Rc::clone(&estado);
        move || {
            let ui = ui_weak.unwrap();
            if let Some(id) = estado.borrow().sessao_aberta {
                let _ = db.definir_status(id, StatusSessao::Realizada);
                ui.set_status_nota("Sessão marcada como realizada.".into());
            }
        }
    });

    // ---------- financeiro ----------
    ui.on_alternar_pago({
        let ui_weak = ui.as_weak();
        let db = Rc::clone(&db);
        let estado = Rc::clone(&estado);
        move |id| {
            let ui = ui_weak.unwrap();
            let _ = db.alternar_pago(id as i64);
            let (ano, mes) = {
                let e = estado.borrow();
                (e.ano, e.mes)
            };
            recarregar_financeiro(&ui, &db, ano, mes);
        }
    });

    // ---------- prontuário ----------
    ui.on_carregar_prontuario({
        let ui_weak = ui.as_weak();
        let db = Rc::clone(&db);
        let estado = Rc::clone(&estado);
        move |indice| {
            let ui = ui_weak.unwrap();
            let paciente_id = {
                let e = estado.borrow();
                e.paciente_ids.get(indice.max(0) as usize).copied()
            };
            if let Some(pid) = paciente_id {
                recarregar_prontuario(&ui, &db, pid);
            }
        }
    });

    // ---------- smart notes: gravação ----------
    ui.on_alternar_gravacao({
        let ui_weak = ui.as_weak();
        let db = Rc::clone(&db);
        let estado = Rc::clone(&estado);
        move |fonte_idx, mic_idx, saida_idx| {
            let ui = ui_weak.unwrap();
            // Reprodução e gravação não convivem: para o player antes de tudo.
            if let Some(reprodutor) = estado.borrow_mut().reprodutor.take() {
                reprodutor.parar();
                ui.set_reproduzindo(false);
            }
            let gravador_ativo = estado.borrow_mut().gravador.take();
            if let Some(gravador) = gravador_ativo {
                ui.set_status_nota("Finalizando gravação e mixando as fontes…".into());
                match gravador.parar() {
                    Ok(()) => {
                        ui.set_status_nota(
                            "Gravação finalizada. Ouça para conferir ou clique em \"Transcrever (local)\"."
                                .into(),
                        );
                        let sessao_aberta = estado.borrow().sessao_aberta;
                        if let Some(sessao_id) = sessao_aberta {
                            let duracao = atualizar_info_audio(&ui, &db, sessao_id);
                            estado.borrow_mut().duracao_audio_s = duracao.unwrap_or(0.0);
                        }
                    }
                    Err(e) => ui.set_status_nota(format!("Erro ao finalizar gravação: {e}").into()),
                }
                ui.set_gravando(false);
                return;
            }
            let (sessao_id, microfone, saida) = {
                let e = estado.borrow();
                let Some(id) = e.sessao_aberta else { return };
                // Índice 0 = "Padrão do Windows" → None; demais mapeiam para o nome real.
                let microfone = (mic_idx > 0)
                    .then(|| e.dispositivos_entrada.get(mic_idx as usize - 1).cloned())
                    .flatten();
                let saida = (saida_idx > 0)
                    .then(|| e.dispositivos_saida.get(saida_idx as usize - 1).cloned())
                    .flatten();
                (id, microfone, saida)
            };
            // Ordem das opções na UI: 0 = sessão online (mic + sistema),
            // 1 = presencial (só microfone), 2 = somente o som do computador.
            let fonte = match fonte_idx {
                1 => vinculo_audio::FonteGravacao::Microfone { nome: microfone },
                2 => vinculo_audio::FonteGravacao::SomDoSistema { saida },
                _ => vinculo_audio::FonteGravacao::MicrofoneESistema { microfone, saida },
            };
            let descricao = match fonte_idx {
                1 => "sessão presencial (sua voz e a do paciente pelo microfone)",
                2 => "o som do computador",
                _ => "a sessão online (sua voz + a voz do paciente na chamada)",
            };
            let caminho = dir_dados().join("audio").join(format!("sessao_{sessao_id}.wav"));
            match vinculo_audio::Gravador::iniciar_com_fonte(&caminho, &fonte) {
                Ok(gravador) => {
                    estado.borrow_mut().gravador = Some(gravador);
                    let _ = db.salvar_audio_nota(sessao_id, &caminho.to_string_lossy());
                    ui.set_gravando(true);
                    ui.set_status_nota(
                        format!("Gravando {descricao}… clique novamente para parar.").into(),
                    );
                }
                Err(e) => ui.set_status_nota(format!("Erro ao iniciar gravação: {e}").into()),
            }
        }
    });

    // ---------- conferência da gravação (play/parar) ----------
    ui.on_alternar_reproducao({
        let ui_weak = ui.as_weak();
        let db = Rc::clone(&db);
        let estado = Rc::clone(&estado);
        move || {
            let ui = ui_weak.unwrap();
            if let Some(reprodutor) = estado.borrow_mut().reprodutor.take() {
                reprodutor.parar();
                ui.set_reproduzindo(false);
                return;
            }
            let Some(sessao_id) = estado.borrow().sessao_aberta else {
                return;
            };
            let Some(caminho) = db
                .nota_da_sessao(sessao_id)
                .ok()
                .flatten()
                .and_then(|(audio, _)| audio)
                .map(PathBuf::from)
            else {
                return;
            };
            match vinculo_audio::Reprodutor::iniciar(&caminho) {
                Ok(reprodutor) => {
                    estado.borrow_mut().reprodutor = Some(reprodutor);
                    ui.set_reproduzindo(true);
                }
                Err(e) => ui.set_status_nota(format!("Erro ao reproduzir: {e}").into()),
            }
        }
    });

    // Detecta o fim natural da reprodução e devolve o botão ao estado "Ouvir".
    let temporizador_player = slint::Timer::default();
    temporizador_player.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(400),
        {
            let ui_weak = ui.as_weak();
            let estado = Rc::clone(&estado);
            move || {
                let (terminou, progresso) = {
                    let e = estado.borrow();
                    match e.reprodutor.as_ref() {
                        Some(r) if r.terminou() => (true, 1.0),
                        Some(r) if e.duracao_audio_s > 0.0 => {
                            (false, (r.posicao_segundos() / e.duracao_audio_s).min(1.0))
                        }
                        Some(_) => (false, 0.0),
                        None => return,
                    }
                };
                let Some(ui) = ui_weak.upgrade() else { return };
                if terminou {
                    estado.borrow_mut().reprodutor = None;
                    ui.set_reproduzindo(false);
                    ui.set_progresso_audio(0.0);
                } else {
                    ui.set_progresso_audio(progresso as f32);
                }
            }
        },
    );
    // O timer precisa sobreviver ao retorno desta função (dropá-lo o pararia).
    std::mem::forget(temporizador_player);

    // ---------- download do modelo de transcrição ----------
    ui.on_baixar_modelo({
        let ui_weak = ui.as_weak();
        move || {
            let ui = ui_weak.unwrap();
            ui.set_ocupado(true);
            ui.set_status_nota("Baixando o modelo de transcrição…".into());
            let destino = caminho_modelo();
            let ui_bg = ui.as_weak();
            std::thread::spawn(move || {
                let ui_progresso = ui_bg.clone();
                let resultado = baixar_modelo(&destino, move |pct| {
                    let _ = ui_progresso.upgrade_in_event_loop(move |ui| {
                        ui.set_status_nota(
                            format!("Baixando o modelo de transcrição… {pct}%").into(),
                        );
                    });
                });
                let _ = ui_bg.upgrade_in_event_loop(move |ui| {
                    ui.set_ocupado(false);
                    match resultado {
                        Ok(()) => {
                            ui.set_precisa_modelo(false);
                            ui.set_status_nota(
                                "Transcrição instalada! Clique em \"Transcrever (local)\"."
                                    .into(),
                            );
                        }
                        Err(e) => ui.set_status_nota(
                            format!("Falha no download do modelo: {e}. Verifique a internet e tente novamente.")
                                .into(),
                        ),
                    }
                });
            });
        }
    });

    // ---------- apagar gravação ----------
    ui.on_apagar_gravacao({
        let ui_weak = ui.as_weak();
        let db = Rc::clone(&db);
        let estado = Rc::clone(&estado);
        move || {
            let ui = ui_weak.unwrap();
            if let Some(reprodutor) = estado.borrow_mut().reprodutor.take() {
                reprodutor.parar();
                ui.set_reproduzindo(false);
            }
            let Some(sessao_id) = estado.borrow().sessao_aberta else {
                return;
            };
            let caminho = db
                .nota_da_sessao(sessao_id)
                .ok()
                .flatten()
                .and_then(|(audio, _)| audio)
                .map(PathBuf::from);
            if let Some(p) = caminho {
                let _ = std::fs::remove_file(p);
            }
            let _ = db.remover_audio_nota(sessao_id);
            atualizar_info_audio(&ui, &db, sessao_id);
            estado.borrow_mut().duracao_audio_s = 0.0;
            ui.set_status_nota(
                "Gravação apagada. A transcrição salva (se houver) foi mantida.".into(),
            );
        }
    });

    // ---------- smart notes: transcrição local (thread) ----------
    ui.on_transcrever({
        let ui_weak = ui.as_weak();
        let db = Rc::clone(&db);
        let estado = Rc::clone(&estado);
        move || {
            let ui = ui_weak.unwrap();
            let Some(sessao_id) = estado.borrow().sessao_aberta else {
                return;
            };
            let Some(wav) = db
                .nota_da_sessao(sessao_id)
                .ok()
                .flatten()
                .and_then(|(audio, _)| audio)
                .map(PathBuf::from)
            else {
                ui.set_status_nota("Nenhum áudio gravado para esta sessão.".into());
                return;
            };
            let modelo = caminho_modelo();
            if !modelo.exists() {
                ui.set_precisa_modelo(true);
                ui.set_status_nota(
                    "A transcrição ainda não está instalada neste computador — \
                     clique no botão azul abaixo para instalar (uma única vez)."
                        .into(),
                );
                return;
            }
            ui.set_ocupado(true);
            ui.set_status_nota("Transcrevendo localmente… isso pode levar alguns minutos.".into());
            let ui_bg = ui.as_weak();
            std::thread::spawn(move || {
                let resultado = vinculo_audio::transcrever(&wav, &modelo);
                let _ = ui_bg.upgrade_in_event_loop(move |ui| match resultado {
                    Ok(texto) => ui.invoke_nota_transcrita(texto.into()),
                    Err(e) => ui.invoke_tarefa_falhou(format!("Falha na transcrição: {e}").into()),
                });
            });
        }
    });

    ui.on_nota_transcrita({
        let ui_weak = ui.as_weak();
        let db = Rc::clone(&db);
        let estado = Rc::clone(&estado);
        move |texto| {
            let ui = ui_weak.unwrap();
            if let Some(sessao_id) = estado.borrow().sessao_aberta {
                let _ = db.salvar_transcricao(sessao_id, texto.as_str());
            }
            ui.set_transcricao(texto);
            ui.set_ocupado(false);
            ui.set_status_nota(
                "Transcrição concluída — processada localmente, o áudio não saiu desta máquina."
                    .into(),
            );
        }
    });

    // ---------- IA: rascunho de evolução (thread) ----------
    ui.on_gerar_rascunho({
        let ui_weak = ui.as_weak();
        let db = Rc::clone(&db);
        let estado = Rc::clone(&estado);
        move || {
            let ui = ui_weak.unwrap();
            let transcricao = ui.get_transcricao().to_string();
            if transcricao.trim().is_empty() {
                return;
            }
            let cfg = match vinculo_ai::ConfigIa::carregar() {
                Ok(c) => c,
                Err(e) => {
                    ui.set_status_nota(format!("{e}").into());
                    return;
                }
            };
            let nomes: Vec<String> = db
                .listar_pacientes()
                .unwrap_or_default()
                .into_iter()
                .map(|p| p.nome)
                .collect();
            let refs: Vec<&str> = nomes.iter().map(String::as_str).collect();
            let (anonimizada, mapa) = vinculo_ai::anonimizar(&transcricao, &refs);
            estado.borrow_mut().mapa_anon = mapa;
            ui.set_ocupado(true);
            ui.set_status_nota(
                "Gerando rascunho com IA — os dados foram anonimizados antes do envio…".into(),
            );
            let ui_bg = ui.as_weak();
            std::thread::spawn(move || {
                let resultado = vinculo_ai::gerar_rascunho(&cfg, &anonimizada);
                let _ = ui_bg.upgrade_in_event_loop(move |ui| match resultado {
                    Ok(rascunho) => ui.invoke_rascunho_gerado(rascunho.into()),
                    Err(e) => {
                        ui.invoke_tarefa_falhou(format!("Falha ao gerar rascunho: {e}").into())
                    }
                });
            });
        }
    });

    ui.on_rascunho_gerado({
        let ui_weak = ui.as_weak();
        let estado = Rc::clone(&estado);
        move |texto| {
            let ui = ui_weak.unwrap();
            let mapa = estado.borrow().mapa_anon.clone();
            let restaurado = vinculo_ai::desanonimizar(texto.as_str(), &mapa);
            ui.set_rascunho(restaurado.into());
            ui.set_ocupado(false);
            ui.set_status_nota(
                "Rascunho gerado. Revise e edite — a decisão profissional é sempre sua.".into(),
            );
        }
    });

    ui.on_tarefa_falhou({
        let ui_weak = ui.as_weak();
        move |mensagem| {
            let ui = ui_weak.unwrap();
            ui.set_ocupado(false);
            ui.set_status_nota(mensagem);
        }
    });

    ui.on_salvar_evolucao({
        let ui_weak = ui.as_weak();
        let db = Rc::clone(&db);
        let estado = Rc::clone(&estado);
        move |texto| {
            let ui = ui_weak.unwrap();
            if texto.trim().is_empty() {
                return;
            }
            let (paciente_id, sessao_id) = {
                let e = estado.borrow();
                (e.paciente_da_sessao, e.sessao_aberta)
            };
            let Some(pid) = paciente_id else { return };
            match db.inserir_evolucao(pid, sessao_id, texto.as_str()) {
                Ok(_) => {
                    ui.set_rascunho("".into());
                    ui.set_status_nota("Evolução salva no prontuário.".into());
                }
                Err(e) => ui.set_status_nota(format!("Erro ao salvar: {e}").into()),
            }
        }
    });

    // ---------- exportação de PDF ----------
    ui.on_exportar_evolucao({
        let ui_weak = ui.as_weak();
        let db = Rc::clone(&db);
        move |id| {
            let ui = ui_weak.unwrap();
            let Ok((evolucao, paciente)) = db.evolucao_por_id(id as i64) else {
                ui.set_pront_status("Evolução não encontrada.".into());
                return;
            };
            let data = formatar_data_registro(&evolucao.criado_em).to_string();
            match exportar_pdf(&db, &paciente, &data, &evolucao.conteudo) {
                Ok(caminho) => {
                    ui.set_pront_status(format!("PDF salvo em {}.", caminho.display()).into())
                }
                Err(e) => ui.set_pront_status(format!("Erro ao exportar PDF: {e}").into()),
            }
        }
    });

    ui.on_exportar_rascunho({
        let ui_weak = ui.as_weak();
        let db = Rc::clone(&db);
        let estado = Rc::clone(&estado);
        move |texto| {
            let ui = ui_weak.unwrap();
            let Some(sessao_id) = estado.borrow().sessao_aberta else {
                return;
            };
            let Ok((sessao, paciente)) = db.sessao_com_nome(sessao_id) else {
                return;
            };
            let data = sessao.inicio.format("%d/%m/%Y às %H:%M").to_string();
            match exportar_pdf(&db, &paciente, &data, texto.as_str()) {
                Ok(caminho) => {
                    ui.set_status_nota(format!("PDF salvo em {}.", caminho.display()).into())
                }
                Err(e) => ui.set_status_nota(format!("Erro ao exportar PDF: {e}").into()),
            }
        }
    });

    // ---------- configurações ----------
    ui.on_salvar_perfil({
        let ui_weak = ui.as_weak();
        let db = Rc::clone(&db);
        move |nome, crp, email, telefone| {
            let ui = ui_weak.unwrap();
            let resultado = db
                .definir_config("perfil.nome", nome.trim())
                .and_then(|_| db.definir_config("perfil.crp", crp.trim()))
                .and_then(|_| db.definir_config("perfil.email", email.trim()))
                .and_then(|_| db.definir_config("perfil.telefone", telefone.trim()));
            match resultado {
                Ok(()) => ui.set_cfg_status("Dados do profissional salvos.".into()),
                Err(e) => ui.set_cfg_status(format!("Erro ao salvar: {e}").into()),
            }
        }
    });

    ui.on_salvar_chave_ia({
        let ui_weak = ui.as_weak();
        move |chave| {
            let ui = ui_weak.unwrap();
            match vinculo_ai::salvar_api_key(chave.as_str()) {
                Ok(caminho) => {
                    ui.set_cfg_status(format!("Chave salva em {}.", caminho.display()).into());
                    ui.set_cfg_status_ia(status_chave_ia().into());
                }
                Err(e) => ui.set_cfg_status(format!("Erro ao salvar a chave: {e}").into()),
            }
        }
    });

    ui.on_salvar_workspace_ia({
        let ui_weak = ui.as_weak();
        move |valor| {
            let ui = ui_weak.unwrap();
            match vinculo_ai::salvar_workspace_id(valor.as_str()) {
                Ok(_) => {
                    let msg = if valor.trim().is_empty() {
                        "Workspace ID removido."
                    } else {
                        "Workspace ID salvo."
                    };
                    ui.set_cfg_status(msg.into());
                }
                Err(e) => ui.set_cfg_status(format!("Erro ao salvar o workspace: {e}").into()),
            }
        }
    });

    ui.on_mascarar(|tipo, texto| aplicar_mascara(tipo.as_str(), texto.as_str()).into());
}
