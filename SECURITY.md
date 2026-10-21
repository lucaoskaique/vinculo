# Política de segurança

O Vínculo lida com dados clínicos sensíveis. Levamos vulnerabilidades a sério.

## Reportando uma vulnerabilidade

Não abra issues públicas para vulnerabilidades. Use o
[report privado do GitHub](../../security/advisories/new) ("Report a vulnerability").

## Escopo de interesse especial

- Vazamento de dados clínicos (prontuário, transcrições, áudios)
- Falhas na anonimização enviada à API de IA
- Escrita/leitura de arquivos fora dos diretórios do app

## Estado atual (transparência)

- O banco local é **criptografado em repouso** (SQLCipher; chave derivada da
  senha do profissional via PBKDF2). Bancos legados em texto plano são
  migrados automaticamente na primeira abertura.
- A chave de API ainda é armazenada em texto plano no config.toml local
  (proteção via DPAPI/keychain no roadmap).
- Os áudios de sessão (WAV) ainda não são criptografados em repouso.
