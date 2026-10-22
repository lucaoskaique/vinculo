//! Captura de áudio (cpal + hound) e transcrição local (whisper-rs).
//!
//! - [`Gravador`]: grava microfone e/ou som do sistema (loopback WASAPI) em WAV.
//! - [`listar_entradas`] / [`listar_saidas`]: nomes dos dispositivos disponíveis.
//! - [`FonteGravacao`]: escolha da fonte de gravação (microfone, som do sistema
//!   ou ambos mixados em um único arquivo).
//! - [`transcrever`]: transcreve um WAV para texto (português) usando um
//!   modelo ggml do whisper.cpp (requer a feature `whisper`, ativa por padrão).

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// Escritor WAV compartilhado entre a thread de áudio e o `Gravador`.
type EscritorWav = hound::WavWriter<BufWriter<File>>;
type EscritorCompartilhado = Arc<Mutex<Option<EscritorWav>>>;

/// Nomes dos dispositivos de entrada (microfones) disponíveis.
pub fn listar_entradas() -> Vec<String> {
    let host = cpal::default_host();
    match host.input_devices() {
        Ok(dispositivos) => dispositivos.filter_map(|d| d.name().ok()).collect(),
        Err(_) => Vec::new(),
    }
}

/// Nomes dos dispositivos de saída (alto-falantes/fones) disponíveis para
/// captura loopback (som do sistema).
pub fn listar_saidas() -> Vec<String> {
    let host = cpal::default_host();
    match host.output_devices() {
        Ok(dispositivos) => dispositivos.filter_map(|d| d.name().ok()).collect(),
        Err(_) => Vec::new(),
    }
}

/// Fonte de gravação. `None` em um nome = dispositivo padrão do Windows.
pub enum FonteGravacao {
    /// Apenas o microfone (voz do profissional).
    Microfone { nome: Option<String> },
    /// Apenas o som do sistema via loopback WASAPI (voz do paciente saindo
    /// pelos alto-falantes/fones — Zoom, Google Meet no Chrome, etc.).
    SomDoSistema { saida: Option<String> },
    /// Microfone e som do sistema, mixados em um único WAV ao parar.
    MicrofoneESistema {
        microfone: Option<String>,
        saida: Option<String>,
    },
}

/// Uma captura em andamento: escritor WAV compartilhado + caminho do temporário.
struct Captura {
    escritor: EscritorCompartilhado,
    caminho_tmp: PathBuf,
}

/// Gravador de áudio: captura microfone e/ou som do sistema (loopback WASAPI).
///
/// Durante a gravação, cada fonte é escrita em um WAV temporário próprio, no
/// sample rate/canais nativos do respectivo dispositivo. Em [`Gravador::parar`],
/// os temporários são mixados offline (mono, 16 kHz, i16) no arquivo final.
///
/// Observação: o `cpal::Stream` não é `Send`; crie e destrua o `Gravador`
/// na mesma thread (por exemplo, a thread da UI).
pub struct Gravador {
    /// Streams ativos: capturas e, se houver loopback, o stream de silêncio
    /// que mantém o dispositivo de saída alimentado.
    streams: Vec<cpal::Stream>,
    /// Capturas em andamento (uma por fonte).
    capturas: Vec<Captura>,
    /// Caminho do WAV final (mono, 16 kHz, i16).
    caminho_final: PathBuf,
}

impl Gravador {
    /// Inicia a captura do microfone padrão, gravando WAV em `caminho_wav`.
    ///
    /// Equivale a `iniciar_com_fonte(caminho_wav, &FonteGravacao::Microfone { nome: None })`.
    pub fn iniciar(caminho_wav: &Path) -> Result<Gravador> {
        Self::iniciar_com_fonte(caminho_wav, &FonteGravacao::Microfone { nome: None })
    }

    /// Inicia a gravação a partir da fonte escolhida, produzindo ao final
    /// (em [`Gravador::parar`]) UM único WAV mono 16 kHz i16 em `caminho_wav`.
    pub fn iniciar_com_fonte(caminho_wav: &Path, fonte: &FonteGravacao) -> Result<Gravador> {
        // Garante que os diretórios pais existem.
        if let Some(pai) = caminho_wav.parent() {
            if !pai.as_os_str().is_empty() {
                std::fs::create_dir_all(pai)
                    .with_context(|| format!("falha ao criar o diretório '{}'", pai.display()))?;
            }
        }

        match Self::iniciar_interno(caminho_wav, fonte) {
            Ok(gravador) => Ok(gravador),
            Err(erro) => {
                // Limpa temporários que possam ter sido criados antes da falha.
                let _ = std::fs::remove_file(caminho_temporario(caminho_wav, "mic"));
                let _ = std::fs::remove_file(caminho_temporario(caminho_wav, "loop"));
                Err(erro)
            }
        }
    }

    /// Abre os streams e escritores de acordo com a fonte escolhida.
    fn iniciar_interno(caminho_wav: &Path, fonte: &FonteGravacao) -> Result<Gravador> {
        let host = cpal::default_host();
        let mut streams: Vec<cpal::Stream> = Vec::new();
        let mut capturas: Vec<Captura> = Vec::new();

        // Decompõe a fonte em (microfone?, saída-loopback?).
        let (mic, saida) = match fonte {
            FonteGravacao::Microfone { nome } => (Some(nome.as_deref()), None),
            FonteGravacao::SomDoSistema { saida } => (None, Some(saida.as_deref())),
            FonteGravacao::MicrofoneESistema { microfone, saida } => {
                (Some(microfone.as_deref()), Some(saida.as_deref()))
            }
        };

        // --- Microfone ---
        if let Some(nome_mic) = mic {
            let dispositivo = encontrar_entrada(&host, nome_mic)?;
            let config = dispositivo
                .default_input_config()
                .context("falha ao obter a configuração padrão do microfone")?;

            let caminho_tmp = caminho_temporario(caminho_wav, "mic");
            let escritor = criar_escritor(&caminho_tmp, &config)?;
            let stream = criar_stream_captura(&dispositivo, &config, Arc::clone(&escritor))
                .context("falha ao criar o stream de captura do microfone")?;
            stream
                .play()
                .context("falha ao iniciar a captura do microfone")?;

            streams.push(stream);
            capturas.push(Captura {
                escritor,
                caminho_tmp,
            });
        }

        // --- Som do sistema (loopback WASAPI) ---
        if let Some(nome_saida) = saida {
            let dispositivo = encontrar_saida(&host, nome_saida)?;
            // No host WASAPI, abrir um stream de ENTRADA em um dispositivo de
            // SAÍDA captura o loopback (tudo o que toca nele), usando a
            // configuração de saída padrão do dispositivo.
            let config = dispositivo.default_output_config().context(
                "falha ao obter a configuração padrão do dispositivo de saída (loopback)",
            )?;

            let caminho_tmp = caminho_temporario(caminho_wav, "loop");
            let escritor = criar_escritor(&caminho_tmp, &config)?;
            let stream = criar_stream_captura(&dispositivo, &config, Arc::clone(&escritor))
                .context(
                    "falha ao abrir a captura do som do sistema (loopback WASAPI); \
                     o driver de áudio pode não suportar loopback — \
                     tente gravar apenas com a fonte 'Microfone'",
                )?;
            stream.play().context(
                "falha ao iniciar a captura do som do sistema (loopback WASAPI); \
                 tente gravar apenas com a fonte 'Microfone'",
            )?;

            streams.push(stream);
            capturas.push(Captura {
                escritor,
                caminho_tmp,
            });

            // Mitigação do "silêncio no loopback": o WASAPI pode não entregar
            // callbacks de loopback enquanto nada toca no dispositivo, o que
            // desalinharia a mixagem. Tocamos silêncio contínuo no mesmo
            // dispositivo para mantê-lo sempre ativo. Falha aqui não impede a
            // gravação — apenas avisa (a mixagem pode desalinhar em silêncios).
            match criar_stream_silencio(&dispositivo, &config) {
                Ok(silencio) => match silencio.play() {
                    Ok(()) => streams.push(silencio),
                    Err(erro) => eprintln!(
                        "aviso: falha ao iniciar o stream de silêncio do loopback \
                         (a mixagem pode desalinhar durante silêncios): {erro}"
                    ),
                },
                Err(erro) => eprintln!(
                    "aviso: falha ao criar o stream de silêncio do loopback \
                     (a mixagem pode desalinhar durante silêncios): {erro}"
                ),
            }
        }

        Ok(Gravador {
            streams,
            capturas,
            caminho_final: caminho_wav.to_path_buf(),
        })
    }

    /// Para os streams e finaliza os escritores temporários, retornando uma
    /// [`TarefaMixagem`] que pode ser executada em uma thread separada.
    ///
    /// Deve ser chamado na mesma thread onde o `Gravador` foi criado (due à
    /// restrição `!Send` de `cpal::Stream`). A `TarefaMixagem` retornada é
    /// `Send` e pode ser passada para outra thread para não bloquear a UI.
    pub fn parar(self) -> Result<TarefaMixagem> {
        // Para todos os streams antes de finalizar os arquivos.
        drop(self.streams);

        // Finaliza os escritores temporários.
        let mut caminhos_tmp: Vec<PathBuf> = Vec::new();
        for captura in &self.capturas {
            let escritor = captura
                .escritor
                .lock()
                .map_err(|_| anyhow!("mutex do escritor WAV envenenado"))?
                .take();
            match escritor {
                Some(w) => w.finalize().with_context(|| {
                    format!(
                        "falha ao finalizar o arquivo WAV temporário '{}'",
                        captura.caminho_tmp.display()
                    )
                })?,
                None => {
                    return Err(anyhow!("o arquivo WAV já havia sido finalizado"));
                }
            }
            caminhos_tmp.push(captura.caminho_tmp.clone());
        }

        Ok(TarefaMixagem {
            caminhos_tmp,
            caminho_final: self.caminho_final,
        })
    }
}

/// Tarefa de mixagem e escrita do WAV final, separada da parada dos streams.
///
/// Criada por [`Gravador::parar`] depois que os streams foram parados e os
/// escritores temporários finalizados. Como é `Send`, pode ser movida para
/// outra thread e executada sem bloquear a UI.
pub struct TarefaMixagem {
    caminhos_tmp: Vec<PathBuf>,
    caminho_final: PathBuf,
}

impl TarefaMixagem {
    /// Mixa as fontes temporárias e escreve o WAV final (mono, 16 kHz, i16).
    ///
    /// Operação pesada: carrega os arquivos temporários na memória, faz
    /// reamostragem e mixagem, e escreve o resultado em disco. Execute em
    /// uma thread separada para não bloquear a UI.
    pub fn executar(self) -> Result<()> {
        let mut mixado: Vec<f32> = Vec::new();
        for caminho in &self.caminhos_tmp {
            let amostras = carregar_wav_mono_16k(caminho).with_context(|| {
                format!(
                    "falha ao carregar o WAV temporário '{}' para mixagem",
                    caminho.display()
                )
            })?;
            mixado = mixar(&mixado, &amostras);
        }

        escrever_wav_mono_16k(&self.caminho_final, &mixado)?;

        for caminho in &self.caminhos_tmp {
            let _ = std::fs::remove_file(caminho);
        }

        Ok(())
    }
}

/// Caminho do WAV temporário de uma fonte (`caminho_final` + `.<rotulo>.tmp.wav`).
fn caminho_temporario(caminho_final: &Path, rotulo: &str) -> PathBuf {
    let mut nome = caminho_final.as_os_str().to_os_string();
    nome.push(format!(".{rotulo}.tmp.wav"));
    PathBuf::from(nome)
}

/// Encontra um dispositivo de entrada pelo nome (`None` = padrão do Windows).
fn encontrar_entrada(host: &cpal::Host, nome: Option<&str>) -> Result<cpal::Device> {
    match nome {
        None => host.default_input_device().ok_or_else(|| {
            anyhow!("nenhum dispositivo de entrada de áudio (microfone) encontrado")
        }),
        Some(nome) => host
            .input_devices()
            .context("falha ao enumerar os dispositivos de entrada de áudio")?
            .find(|d| d.name().map(|n| n == nome).unwrap_or(false))
            .ok_or_else(|| {
                anyhow!(
                    "dispositivo de entrada (microfone) '{nome}' não encontrado; \
                     use listar_entradas() para ver os nomes disponíveis"
                )
            }),
    }
}

/// Encontra um dispositivo de saída pelo nome (`None` = padrão do Windows).
fn encontrar_saida(host: &cpal::Host, nome: Option<&str>) -> Result<cpal::Device> {
    match nome {
        None => host.default_output_device().ok_or_else(|| {
            anyhow!("nenhum dispositivo de saída de áudio (alto-falantes/fones) encontrado")
        }),
        Some(nome) => host
            .output_devices()
            .context("falha ao enumerar os dispositivos de saída de áudio")?
            .find(|d| d.name().map(|n| n == nome).unwrap_or(false))
            .ok_or_else(|| {
                anyhow!(
                    "dispositivo de saída '{nome}' não encontrado; \
                     use listar_saidas() para ver os nomes disponíveis"
                )
            }),
    }
}

/// Cria o escritor WAV de uma captura (PCM i16, sample rate/canais nativos
/// do dispositivo).
fn criar_escritor(
    caminho: &Path,
    config: &cpal::SupportedStreamConfig,
) -> Result<EscritorCompartilhado> {
    let spec = hound::WavSpec {
        channels: config.channels(),
        sample_rate: config.sample_rate().0,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let escritor = hound::WavWriter::create(caminho, spec)
        .with_context(|| format!("falha ao criar o arquivo WAV '{}'", caminho.display()))?;
    Ok(Arc::new(Mutex::new(Some(escritor))))
}

/// Cria um stream de captura (entrada ou loopback) que escreve as amostras,
/// convertidas para i16, no escritor compartilhado.
fn criar_stream_captura(
    dispositivo: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    escritor: EscritorCompartilhado,
) -> Result<cpal::Stream> {
    let formato = config.sample_format();
    let config_stream: cpal::StreamConfig = config.clone().into();
    let ao_errar = |erro: cpal::StreamError| {
        eprintln!("erro no stream de áudio: {erro}");
    };

    let stream = match formato {
        cpal::SampleFormat::F32 => dispositivo.build_input_stream(
            &config_stream,
            move |dados: &[f32], _: &cpal::InputCallbackInfo| {
                escrever_amostras(&escritor, dados, |a| {
                    (a.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
                });
            },
            ao_errar,
            None,
        ),
        cpal::SampleFormat::I16 => dispositivo.build_input_stream(
            &config_stream,
            move |dados: &[i16], _: &cpal::InputCallbackInfo| {
                escrever_amostras(&escritor, dados, |a| a);
            },
            ao_errar,
            None,
        ),
        cpal::SampleFormat::U16 => dispositivo.build_input_stream(
            &config_stream,
            move |dados: &[u16], _: &cpal::InputCallbackInfo| {
                escrever_amostras(&escritor, dados, |a| (a as i32 - 32768) as i16);
            },
            ao_errar,
            None,
        ),
        outro => {
            return Err(anyhow!(
                "formato de amostra não suportado pelo gravador: {outro:?}"
            ))
        }
    }
    .context("falha ao criar o stream de captura de áudio")?;

    Ok(stream)
}

/// Cria um stream de SAÍDA que toca silêncio contínuo no dispositivo,
/// mantendo o loopback WASAPI alimentado mesmo quando nada mais está tocando.
fn criar_stream_silencio(
    dispositivo: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
) -> Result<cpal::Stream> {
    let formato = config.sample_format();
    let config_stream: cpal::StreamConfig = config.clone().into();
    let ao_errar = |erro: cpal::StreamError| {
        eprintln!("erro no stream de silêncio do loopback: {erro}");
    };

    let stream = match formato {
        cpal::SampleFormat::F32 => dispositivo.build_output_stream(
            &config_stream,
            move |dados: &mut [f32], _: &cpal::OutputCallbackInfo| {
                dados.fill(0.0);
            },
            ao_errar,
            None,
        ),
        cpal::SampleFormat::I16 => dispositivo.build_output_stream(
            &config_stream,
            move |dados: &mut [i16], _: &cpal::OutputCallbackInfo| {
                dados.fill(0);
            },
            ao_errar,
            None,
        ),
        cpal::SampleFormat::U16 => dispositivo.build_output_stream(
            &config_stream,
            move |dados: &mut [u16], _: &cpal::OutputCallbackInfo| {
                // Ponto de equilíbrio (silêncio) para amostras u16.
                dados.fill(32768);
            },
            ao_errar,
            None,
        ),
        outro => {
            return Err(anyhow!(
                "formato de amostra não suportado pelo stream de silêncio: {outro:?}"
            ))
        }
    }
    .context("falha ao criar o stream de silêncio do loopback")?;

    Ok(stream)
}

/// Soma duas pistas mono amostra a amostra com saturação em [-1, 1].
/// O comprimento do resultado é o maior dos dois.
fn mixar(a: &[f32], b: &[f32]) -> Vec<f32> {
    let n = a.len().max(b.len());
    (0..n)
        .map(|i| {
            let x = a.get(i).copied().unwrap_or(0.0);
            let y = b.get(i).copied().unwrap_or(0.0);
            (x + y).clamp(-1.0, 1.0)
        })
        .collect()
}

/// Escreve amostras mono f32 em [-1, 1] como WAV mono, 16 kHz, PCM i16.
fn escrever_wav_mono_16k(caminho: &Path, amostras: &[f32]) -> Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut escritor = hound::WavWriter::create(caminho, spec)
        .with_context(|| format!("falha ao criar o arquivo WAV final '{}'", caminho.display()))?;
    for &a in amostras {
        escritor
            .write_sample((a.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
            .context("falha ao escrever amostra no WAV final")?;
    }
    escritor
        .finalize()
        .context("falha ao finalizar o arquivo WAV final")?;
    Ok(())
}

/// Escreve um bloco de amostras no WAV, convertendo cada amostra para i16.
fn escrever_amostras<T: Copy>(
    escritor: &EscritorCompartilhado,
    dados: &[T],
    converter: impl Fn(T) -> i16,
) {
    if let Ok(mut guarda) = escritor.lock() {
        if let Some(w) = guarda.as_mut() {
            for &amostra in dados {
                // Erros de escrita no callback de áudio são ignorados de propósito
                // (não há como propagá-los; o finalize() reportará problemas graves).
                let _ = w.write_sample(converter(amostra));
            }
        }
    }
}

/// Transcreve um WAV para texto (português) usando um modelo ggml do whisper.cpp.
pub fn transcrever(caminho_wav: &Path, caminho_modelo: &Path) -> Result<String> {
    if !caminho_modelo.exists() {
        return Err(anyhow!(
            "modelo whisper não encontrado em '{}'; baixe um modelo ggml do whisper.cpp \
             (ex.: ggml-base.bin) e coloque-o nesse caminho",
            caminho_modelo.display()
        ));
    }

    let amostras = carregar_wav_mono_16k(caminho_wav)?;

    #[cfg(feature = "whisper")]
    {
        transcrever_whisper(&amostras, caminho_modelo)
    }

    #[cfg(not(feature = "whisper"))]
    {
        let _ = amostras;
        Err(anyhow!(
            "transcrição local não está disponível neste build: a crate vinculo-audio \
             foi compilada sem a feature `whisper` (whisper.cpp não pôde ser compilado \
             neste ambiente)"
        ))
    }
}

/// Carrega um WAV, converte para mono f32 normalizado em [-1, 1] e
/// reamostra para 16000 Hz (taxa exigida pelo whisper e usada na mixagem).
fn carregar_wav_mono_16k(caminho: &Path) -> Result<Vec<f32>> {
    let leitor = hound::WavReader::open(caminho)
        .with_context(|| format!("falha ao abrir o arquivo WAV '{}'", caminho.display()))?;
    let spec = leitor.spec();
    let canais = spec.channels.max(1) as usize;

    // Lê todas as amostras intercaladas como f32 em [-1, 1].
    let brutas: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => leitor
            .into_samples::<f32>()
            .collect::<std::result::Result<_, _>>()
            .context("falha ao ler amostras float do WAV")?,
        hound::SampleFormat::Int => {
            let escala = (1i64 << (spec.bits_per_sample - 1)) as f32;
            leitor
                .into_samples::<i32>()
                .map(|s| s.map(|v| v as f32 / escala))
                .collect::<std::result::Result<_, _>>()
                .context("falha ao ler amostras inteiras do WAV")?
        }
    };

    // Mistura para mono (média dos canais).
    let mono: Vec<f32> = if canais == 1 {
        brutas
    } else {
        brutas
            .chunks(canais)
            .map(|quadro| quadro.iter().sum::<f32>() / quadro.len() as f32)
            .collect()
    };

    Ok(reamostrar_linear(&mono, spec.sample_rate, 16_000))
}

/// Reamostragem linear simples de `sr_origem` para `sr_destino`.
fn reamostrar_linear(entrada: &[f32], sr_origem: u32, sr_destino: u32) -> Vec<f32> {
    if sr_origem == sr_destino || entrada.is_empty() {
        return entrada.to_vec();
    }
    let razao = sr_origem as f64 / sr_destino as f64;
    let n_saida = ((entrada.len() as f64) / razao).round() as usize;
    let ultimo = entrada.len() - 1;
    let mut saida = Vec::with_capacity(n_saida);
    for i in 0..n_saida {
        let pos = i as f64 * razao;
        let idx = pos as usize;
        let fracao = (pos - idx as f64) as f32;
        let a = entrada[idx.min(ultimo)];
        let b = entrada[(idx + 1).min(ultimo)];
        saida.push(a + (b - a) * fracao);
    }
    saida
}

/// Executa o whisper.cpp sobre as amostras (mono, 16 kHz, f32) e concatena os segmentos.
#[cfg(feature = "whisper")]
fn transcrever_whisper(amostras: &[f32], caminho_modelo: &Path) -> Result<String> {
    use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

    let caminho_modelo_str = caminho_modelo
        .to_str()
        .ok_or_else(|| anyhow!("caminho do modelo contém caracteres inválidos (não UTF-8)"))?;

    let contexto =
        WhisperContext::new_with_params(caminho_modelo_str, WhisperContextParameters::default())
            .with_context(|| {
                format!(
                    "falha ao carregar o modelo whisper em '{}'",
                    caminho_modelo.display()
                )
            })?;

    let mut estado = contexto
        .create_state()
        .context("falha ao criar o estado do whisper")?;

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(Some("pt"));
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);

    estado
        .full(params, amostras)
        .context("falha ao executar a transcrição (whisper full)")?;

    let n_segmentos = estado.full_n_segments();
    let mut texto = String::new();
    for i in 0..n_segmentos {
        let segmento = estado
            .get_segment(i)
            .ok_or_else(|| anyhow!("segmento {i} fora dos limites"))?;
        let trecho = segmento
            .to_str_lossy()
            .with_context(|| format!("falha ao obter o texto do segmento {i}"))?;
        let trecho = trecho.trim();
        if trecho.is_empty() {
            continue;
        }
        if !texto.is_empty() {
            texto.push(' ');
        }
        texto.push_str(trecho);
    }

    Ok(texto)
}

// ---------------------------------------------------------------------------
// Reprodução (conferência da gravação)
// ---------------------------------------------------------------------------

/// Reprodutor simples para conferir a gravação da sessão.
///
/// Assim como o `Gravador`, deve ser criado e destruído na mesma thread
/// (a `OutputStream` do rodio não é `Send`).
pub struct Reprodutor {
    _stream: rodio::OutputStream,
    sink: rodio::Sink,
}

impl Reprodutor {
    /// Inicia a reprodução do WAV no dispositivo de saída padrão.
    pub fn iniciar(caminho: &Path) -> Result<Reprodutor> {
        let (stream, handle) = rodio::OutputStream::try_default()
            .map_err(|e| anyhow!("falha ao abrir o dispositivo de reprodução: {e}"))?;
        let sink = rodio::Sink::try_new(&handle)
            .map_err(|e| anyhow!("falha ao preparar a reprodução: {e}"))?;
        let arquivo = std::fs::File::open(caminho)
            .with_context(|| format!("falha ao abrir a gravação '{}'", caminho.display()))?;
        let fonte = rodio::Decoder::new(std::io::BufReader::new(arquivo))
            .map_err(|e| anyhow!("falha ao decodificar a gravação: {e}"))?;
        sink.append(fonte);
        sink.play();
        Ok(Reprodutor {
            _stream: stream,
            sink,
        })
    }

    /// `true` quando a reprodução chegou ao fim.
    pub fn terminou(&self) -> bool {
        self.sink.empty()
    }

    /// Interrompe a reprodução.
    pub fn parar(self) {
        self.sink.stop();
    }
}

/// Duração de um WAV em segundos (pela especificação do arquivo).
pub fn duracao_wav(caminho: &Path) -> Result<f64> {
    let leitor = hound::WavReader::open(caminho)
        .with_context(|| format!("falha ao abrir '{}'", caminho.display()))?;
    let spec = leitor.spec();
    Ok(leitor.duration() as f64 / spec.sample_rate.max(1) as f64)
}

impl Reprodutor {
    /// Posição atual da reprodução, em segundos.
    pub fn posicao_segundos(&self) -> f64 {
        self.sink.get_pos().as_secs_f64()
    }
}

/// Picos de amplitude do WAV em `colunas` colunas, normalizados em [0, 1],
/// para desenhar a forma de onda. Processa em streaming (não carrega o
/// arquivo inteiro na memória).
pub fn forma_de_onda(caminho: &Path, colunas: usize) -> Result<Vec<f32>> {
    let mut leitor = hound::WavReader::open(caminho)
        .with_context(|| format!("falha ao abrir '{}'", caminho.display()))?;
    let spec = leitor.spec();
    let colunas = colunas.max(1);
    let canais = spec.channels.max(1) as usize;
    let total_quadros = (leitor.duration() as usize).max(1);
    let quadros_por_coluna = (total_quadros / colunas).max(1);
    let mut picos = vec![0f32; colunas];

    let mut registrar = |indice_amostra: usize, valor: f32| {
        let quadro = indice_amostra / canais;
        let coluna = (quadro / quadros_por_coluna).min(colunas - 1);
        let v = valor.abs();
        if v > picos[coluna] {
            picos[coluna] = v;
        }
    };

    match spec.sample_format {
        hound::SampleFormat::Float => {
            for (i, amostra) in leitor.samples::<f32>().enumerate() {
                registrar(i, amostra.context("falha ao ler amostras do WAV")?);
            }
        }
        hound::SampleFormat::Int => {
            let escala = (1i64 << (spec.bits_per_sample - 1)) as f32;
            for (i, amostra) in leitor.samples::<i32>().enumerate() {
                registrar(
                    i,
                    amostra.context("falha ao ler amostras do WAV")? as f32 / escala,
                );
            }
        }
    }

    // Normaliza pelo pico global para a forma de onda preencher a área.
    let maior = picos.iter().cloned().fold(0f32, f32::max);
    if maior > 0.0 {
        for p in &mut picos {
            *p /= maior;
        }
    }
    Ok(picos)
}
