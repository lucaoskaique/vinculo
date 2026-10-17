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

- O banco local ainda **não** é criptografado em repouso (SQLCipher no roadmap).
- A chave de API é armazenada em texto plano no config.toml local.
- Por isso, o app ainda não deve ser usado com dados clínicos reais.
