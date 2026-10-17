CREATE TABLE IF NOT EXISTS pacientes (
    id         INTEGER PRIMARY KEY,
    nome       TEXT NOT NULL,
    telefone   TEXT,
    email      TEXT,
    criado_em  TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS sessoes (
    id              INTEGER PRIMARY KEY,
    paciente_id     INTEGER NOT NULL REFERENCES pacientes(id),
    inicio          TEXT NOT NULL,               -- ISO 8601, hora local do consultório
    duracao_min     INTEGER NOT NULL DEFAULT 50,
    status          TEXT NOT NULL DEFAULT 'agendada',
    valor_centavos  INTEGER NOT NULL DEFAULT 0,
    pago            INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_sessoes_inicio ON sessoes (inicio);

CREATE TABLE IF NOT EXISTS smart_notes (
    id           INTEGER PRIMARY KEY,
    sessao_id    INTEGER NOT NULL UNIQUE REFERENCES sessoes(id),
    audio_path   TEXT,
    transcricao  TEXT,
    criado_em    TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS evolucoes (
    id           INTEGER PRIMARY KEY,
    paciente_id  INTEGER NOT NULL REFERENCES pacientes(id),
    sessao_id    INTEGER REFERENCES sessoes(id),
    conteudo     TEXT NOT NULL,
    criado_em    TEXT NOT NULL DEFAULT (datetime('now', 'localtime'))
);

CREATE INDEX IF NOT EXISTS idx_evolucoes_paciente ON evolucoes (paciente_id);

CREATE TABLE IF NOT EXISTS config (
    chave  TEXT PRIMARY KEY,
    valor  TEXT NOT NULL
);
