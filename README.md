# Vínculo

Gestão clínica para psicólogos, **local-first**: agenda, prontuário, financeiro,
gravação e transcrição de sessões e rascunhos de evolução com IA — com os dados
clínicos guardados **na máquina do profissional**, não na nuvem.

Feito em Rust + [Slint](https://slint.dev).

## Por que local-first?

- **O áudio da sessão nunca sai do computador** — a transcrição usa o
  [whisper.cpp](https://github.com/ggerganov/whisper.cpp) rodando localmente.
- Funciona **offline** (agenda, prontuário, gravação, transcrição, PDF).
- Ao gerar rascunhos com IA (Claude API), a transcrição é **anonimizada antes do
  envio** (nomes, e-mails, telefones e CPFs viram placeholders) e o resultado é
  sempre revisado pelo profissional antes de entrar no prontuário.
- Os dados ficam num SQLite do próprio usuário — backup e portabilidade triviais.

## Funcionalidades

- **Agenda** — sessões por dia, criação rápida, status e pagamento
- **Pacientes** — cadastro e contatos
- **Prontuário** — evoluções clínicas por paciente, com exportação em PDF
- **Financeiro** — visão mensal, totais recebido/pendente
- **Smart Notes** — gravação da sessão (presencial pelo microfone, ou online
  capturando microfone + som do sistema: Zoom, Meet, Teams…), forma de onda,
  conferência com player, transcrição local em português
- **IA** — rascunho de evolução estruturado (temas, intervenções, evolução,
  combinados), gerado a partir da transcrição anonimizada e sempre editável
- **PDF** — anotação de sessão com layout de relatório clínico

## Download

Binários para Windows, macOS e Linux na página de
[Releases](../../releases). No primeiro uso da transcrição, baixe um modelo do
whisper.cpp (ex.: [ggml-base.bin](https://huggingface.co/ggerganov/whisper.cpp/tree/main))
e salve no caminho que o app indicar.

> Captura do som do sistema (sessão online) está disponível no Windows (WASAPI).
> No Linux use o dispositivo "Monitor of …" como microfone; no macOS é necessário
> um driver virtual como o BlackHole. Veja o TODO.md.

## Compilando do código

Requisitos: Rust estável, CMake e um libclang disponível (para o bindgen do
whisper). No Linux, também: `libasound2-dev libssl-dev libfontconfig1-dev pkg-config clang`.

```sh
cargo build --release -p vinculo
```

## Licenças de terceiros

- Fontes DejaVu embutidas no PDF: ver `crates/relatorio/fonts/LICENSE-DejaVu.txt`
- whisper.cpp (MIT), via [whisper-rs](https://github.com/tazz4843/whisper-rs)

## Status

Em desenvolvimento ativo — ainda **não** usar com dados clínicos reais
(criptografia do banco em repouso está no roadmap; ver `TODO.md`).
Todos os direitos reservados.
