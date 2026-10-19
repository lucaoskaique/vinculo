# TODO — Vínculo

## Diferenciais vs Sintropia (tese: soberania sobre o dado clínico)

A vantagem estrutural é local-first: áudio nunca sai da máquina (Whisper local),
funciona offline, captura som do sistema (inviável em app web), dado em SQLite do
profissional, preço sem mensalidade eterna. As features abaixo aprofundam isso —
em ordem de prioridade:

- [ ] **IA 100% local (Ollama/llama.cpp)** — gerar o rascunho de evolução sem internet
      e sem nenhum dado sair da máquina (nem anonimizado); elimina custo de API.
      Feature-assassina da tese: nenhum SaaS consegue oferecer. Fallback/opção ao
      lado da Claude API na tela de Configurações.
- [ ] **Linha do tempo do paciente / análise longitudinal** — cruzar todas as sessões:
      temas recorrentes, evolução de queixas, "o que mudou desde a 1ª sessão".
      Clinicamente valioso; na nuvem seria problema de privacidade, aqui é uma query.
- [ ] **Busca full-text local** em transcrições e evoluções ("em que sessão ele falou
      da mãe?") — FTS5 do SQLite, instantânea.
- [ ] **Documentos do CFP** — declaração, atestado, relatório/laudo psicológico nos
      modelos das resoluções do Conselho, pré-preenchidos com prontuário + nome/CRP
      das Configurações (reutilizar a crate vinculo-relatorio).
- [ ] **Modo supervisão** — exportar estudo de caso agressivamente anonimizado para
      supervisão clínica.
- [ ] **Escalas e instrumentos** (PHQ-9, GAD-7, BDI) aplicados e pontuados no app,
      com gráfico de evolução integrado à linha do tempo.
- [ ] **Diarização de falantes** (terapeuta vs paciente) na transcrição local — difícil,
      mas elevaria muito a qualidade do rascunho da IA.

Onde o Sintropia ganha (trade-off consciente do posicionamento, não brigar agora):
app do paciente iOS, lembretes automáticos WhatsApp, acesso multi-dispositivo/sync.

## Gravação: capturar áudio por aplicativo (estilo OBS)

**Motivação:** sons do sistema (notificações do WhatsApp/e-mail, outros apps) entram na
gravação da sessão quando capturamos o dispositivo de saída inteiro. Isolar a captura
no app da chamada (Zoom, Meet no Chrome, Teams…) eliminaria esse ruído — como o OBS
faz com "Captura de áudio do aplicativo".

**Caminho técnico:**
- Windows 10 2004+ tem *process loopback* (WASAPI): `ActivateAudioInterfaceAsync` com
  `AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK`, capturando um processo **e seus filhos**
  (a captura por árvore de processos resolve o caso do Chrome, que toca áudio num renderer).
- O cpal **não** suporta isso — exigiria implementação WASAPI direta via `windows-rs`
  dentro da `vinculo-audio`, convivendo com o pipeline atual (WAV temporário + mixagem).
- UX: listar apps que estão emitindo áudio (sessão de áudio ativa) em vez de "janelas",
  como opção avançada — o padrão continua sendo o dispositivo inteiro (à prova de erro).

**Alternativa barata para fazer antes (mitigação):**
- Sugerir na dica da tela de gravação ativar o "Não incomodar" do Windows durante a sessão;
  idealmente, ativar/desativar automaticamente ao iniciar/parar a gravação
  (API `WNF`/Focus Assist não é pública-estável; investigar `quiethours`/Settings URI).

## Multiplataforma (Windows / macOS / Linux)

Código já portável: caminhos via `dirs` (core::dir_dados/dir_documentos), PDF abre via
crate `open`, fontes DejaVu embutidas no binário (Segoe UI só como preferência no Windows).

Pendências por plataforma:
- [ ] **Captura do som do sistema fora do Windows**: o loopback atual é WASAPI.
      - Linux: monitores do PulseAudio/PipeWire ("Monitor of …") já aparecem como
        *entrada* — detectar e oferecer no seletor como "som do sistema".
      - macOS: não há loopback nativo; exigir driver virtual (BlackHole) e orientar o
        usuário, ou integrar ScreenCaptureKit (macOS 13+) via bindings.
      - O código já falha com mensagem clara sugerindo a fonte "Microfone".
- [ ] Testar/compilar em macOS e Linux de verdade (CI com matriz dos 3 SOs);
      documentar deps de build (cmake + libclang para o whisper).
- [ ] Fonte da UI: hoje pede "Segoe UI Variable Text"; nos outros SOs o Slint cai na
      fonte padrão do sistema (ok), mas vale definir por plataforma.

## Auditoria RustSec (job informativo do CI)

- [ ] **Migrar a crate vinculo-relatorio para uma lib de PDF mantida** (printpdf
      moderno direto, krilla ou typst). O genpdf 0.2 arrasta printpdf 0.3 com:
      - `lopdf` RUSTSEC-2026-0187 (stack overflow ao *parsear* PDFs aninhados) —
        **risco aceito por ora**: o Vínculo só escreve PDFs, nunca abre PDFs de
        terceiros; não há superfície de ataque enquanto isso valer.
      - 4 crates sem manutenção (bincode, encoding, lzw, rusttype), transitivas.
      Quando migrar, tornar o job `auditoria` bloqueante no CI.

## Outros pendentes
- [ ] SQLCipher / criptografia em repouso do banco — **antes de dados clínicos reais**.
- [ ] Esconder seletores de dispositivo atrás de "Configurações avançadas de áudio".
- [ ] Download automático do modelo Whisper (ggml-base.bin) na primeira utilização.
- [ ] Sincronização com Google Calendar.
- [ ] Lembretes de sessão via WhatsApp (link wa.me pré-preenchido numa v1).
- [ ] App/formulário do paciente (cartão diário de emoções, tarefas, escalas).
- [ ] Compensação de drift de clock na mixagem mic+loopback (gravações muito longas).
- [ ] Primeiro commit do repositório git.
