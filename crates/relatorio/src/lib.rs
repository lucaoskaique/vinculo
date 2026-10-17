//! Geração do PDF de anotação de sessão (estilo relatório clínico).
//!
//! Layout inspirado em relatórios clínicos: cabeçalho com título "Anotação",
//! dados do paciente/sessão e a marca "Vínculo"; corpo com seções e bullets
//! a partir de um markdown leve; rodapé em todas as páginas com crédito e
//! numeração "Página X de Y" (obtida via renderização em duas passadas).

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Context as _, Result};
use genpdf::elements::{Break, BulletPoint, Paragraph, TableLayout};
use genpdf::error::Error as ErroGenpdf;
use genpdf::fonts::{self, FontData, FontFamily};
use genpdf::render::Area;
use genpdf::style::{Color, Style};
use genpdf::{
    Alignment, Context, Document, Element, Margins, Mm, PaperSize, Position, RenderResult, Size,
};

/// Dados necessários para montar o PDF de uma anotação de sessão.
pub struct DadosAnotacao<'a> {
    pub paciente: &'a str,
    /// Já formatada para exibição, ex.: "07/10/2026 às 16:00".
    pub data_sessao: &'a str,
    /// Nome do profissional (pode ser vazio).
    pub profissional: &'a str,
    /// Registro profissional, ex.: "CRP 05/12345" (pode ser vazio).
    pub crp: &'a str,
    /// Conteúdo em markdown leve (##, ###, "- " e rótulos "Resumo:" etc.).
    pub conteudo_markdown: &'a str,
}

// ---------------------------------------------------------------------------
// Paleta e constantes de estilo
// ---------------------------------------------------------------------------

/// Quase-preto para o texto corrido.
const COR_TEXTO: Color = Color::Rgb(29, 29, 31);
/// Azul de destaque para títulos de seção e para a marca.
const COR_SECAO: Color = Color::Rgb(10, 132, 255);
/// Azul acinzentado, mais suave, para subtítulos de tema.
const COR_SUBSECAO: Color = Color::Rgb(84, 119, 161);
/// Cinza discreto para o rodapé.
const COR_RODAPE: Color = Color::Rgb(128, 128, 133);
/// Cinza claro para as linhas separadoras.
const COR_LINHA: Color = Color::Rgb(200, 200, 205);

/// Margem lateral/vertical padrão da página, em milímetros.
const MARGEM_MM: f64 = 20.0;
/// Texto fixo do rodapé (lado esquerdo).
const TEXTO_RODAPE: &str = "Gerado com Vínculo · rascunho revisado pelo profissional";

/// Rótulos conhecidos que devem aparecer em negrito no início do parágrafo.
const ROTULOS: [&str; 3] = ["Resumo:", "Pontos registrados:", "Leitura pela abordagem:"];

/// Estilo base do texto corrido (11pt, quase-preto, entrelinha ~1.3).
fn estilo_corpo() -> Style {
    Style::new()
        .with_font_size(11)
        .with_color(COR_TEXTO)
        .with_line_spacing(1.3)
}

// ---------------------------------------------------------------------------
// Fontes
// ---------------------------------------------------------------------------

/// Carrega as fontes do relatório: no Windows tenta a Segoe UI do sistema
/// (visual nativo); em qualquer plataforma — ou se a Segoe UI faltar — usa a
/// DejaVu Sans embutida no binário, garantindo funcionamento em Windows,
/// macOS e Linux sem depender de fontes instaladas.
fn carregar_fontes() -> Result<FontFamily<FontData>> {
    if cfg!(windows) {
        if let Ok(familia) = fontes_segoe() {
            return Ok(familia);
        }
    }
    fontes_embutidas()
}

/// Família Segoe UI lida dos arquivos do Windows.
fn fontes_segoe() -> Result<FontFamily<FontData>> {
    let ler = |arquivo: &str, variante: &str| -> Result<FontData> {
        let caminho = Path::new(r"C:\Windows\Fonts").join(arquivo);
        let bytes = std::fs::read(&caminho).with_context(|| {
            format!(
                "Não foi possível ler a fonte Segoe UI ({variante}) em {}.",
                caminho.display()
            )
        })?;
        fonts::FontData::new(bytes, None)
            .map_err(|e| anyhow!("Fonte Segoe UI ({variante}) inválida em {arquivo}: {e:?}"))
    };

    Ok(FontFamily {
        regular: ler("segoeui.ttf", "regular")?,
        bold: ler("segoeuib.ttf", "negrito")?,
        italic: ler("segoeuii.ttf", "itálico")?,
        bold_italic: ler("segoeuiz.ttf", "negrito itálico")?,
    })
}

/// Família DejaVu Sans embutida no binário (fallback multiplataforma).
fn fontes_embutidas() -> Result<FontFamily<FontData>> {
    let criar = |bytes: &'static [u8], variante: &str| -> Result<FontData> {
        fonts::FontData::new(bytes.to_vec(), None)
            .map_err(|e| anyhow!("Fonte embutida DejaVu Sans ({variante}) inválida: {e:?}"))
    };
    Ok(FontFamily {
        regular: criar(include_bytes!("../fonts/DejaVuSans.ttf"), "regular")?,
        bold: criar(include_bytes!("../fonts/DejaVuSans-Bold.ttf"), "negrito")?,
        italic: criar(include_bytes!("../fonts/DejaVuSans-Oblique.ttf"), "itálico")?,
        bold_italic: criar(
            include_bytes!("../fonts/DejaVuSans-BoldOblique.ttf"),
            "negrito itálico",
        )?,
    })
}

// ---------------------------------------------------------------------------
// Decorador de página: margens + rodapé em todas as páginas
// ---------------------------------------------------------------------------

/// Decorador que define as margens e desenha o rodapé em todas as páginas.
///
/// Mantém um contador interno de páginas; na primeira passada (quando
/// `total_paginas` é `None`) o total observado é publicado em `contador`,
/// permitindo renderizar "Página X de Y" na segunda passada.
struct DecoradorRelatorio {
    pagina: usize,
    total_paginas: Option<usize>,
    contador: Option<Arc<AtomicUsize>>,
}

impl genpdf::PageDecorator for DecoradorRelatorio {
    fn decorate_page<'a>(
        &mut self,
        context: &Context,
        mut area: Area<'a>,
        _style: Style,
    ) -> Result<Area<'a>, ErroGenpdf> {
        self.pagina += 1;
        if let Some(contador) = &self.contador {
            contador.store(self.pagina, Ordering::Relaxed);
        }

        let tamanho = area.size();
        let margem = Mm::from(MARGEM_MM);
        let estilo = Style::new().with_font_size(8).with_color(COR_RODAPE);
        // Linha de base do rodapé, próxima ao fim da página.
        let y = tamanho.height - Mm::from(14.0);

        // Linha fina separando o rodapé do conteúdo (no genpdf 0.2 a linha
        // usa apenas a cor do estilo; a espessura é a padrão, bem fina).
        area.draw_line(
            vec![
                Position::new(margem, y - Mm::from(1.2)),
                Position::new(tamanho.width - margem, y - Mm::from(1.2)),
            ],
            Style::new().with_color(COR_LINHA),
        );

        // Esquerda: crédito fixo.
        area.print_str(&context.font_cache, Position::new(margem, y), estilo, TEXTO_RODAPE)?;

        // Direita: numeração de página, alinhada pela largura medida do texto.
        let texto_pagina = match self.total_paginas {
            Some(total) => format!("Página {} de {}", self.pagina, total),
            None => format!("Página {}", self.pagina),
        };
        let largura = estilo.str_width(&context.font_cache, &texto_pagina);
        area.print_str(
            &context.font_cache,
            Position::new(tamanho.width - margem - largura, y),
            estilo,
            &texto_pagina,
        )?;

        // Reserva as margens do conteúdo; a inferior é maior para não
        // sobrepor o rodapé.
        area.add_margins(Margins::trbl(
            MARGEM_MM,
            MARGEM_MM,
            MARGEM_MM + 4.0,
            MARGEM_MM,
        ));
        Ok(area)
    }
}

// ---------------------------------------------------------------------------
// Elemento: linha horizontal
// ---------------------------------------------------------------------------

/// Elemento que desenha uma linha horizontal fina ocupando toda a largura.
struct LinhaHorizontal {
    cor: Color,
}

impl Element for LinhaHorizontal {
    fn render(
        &mut self,
        _context: &Context,
        area: Area<'_>,
        _style: Style,
    ) -> Result<RenderResult, ErroGenpdf> {
        let largura = area.size().width;
        area.draw_line(
            vec![Position::new(0.0, 0.3), Position::new(largura, 0.3)],
            Style::new().with_color(self.cor),
        );
        let mut resultado = RenderResult::default();
        resultado.size = Size::new(largura, Mm::from(0.8));
        Ok(resultado)
    }
}

// ---------------------------------------------------------------------------
// Montagem do documento
// ---------------------------------------------------------------------------

/// Cabeçalho da primeira página: título, marca, metadados e linha separadora.
fn adicionar_cabecalho(doc: &mut Document, dados: &DadosAnotacao) -> Result<()> {
    // Título "Anotação" à esquerda e a marca "Vínculo" à direita.
    let mut topo = TableLayout::new(vec![3, 1]);
    topo.row()
        .element(
            Paragraph::new("Anotação")
                .styled(Style::new().bold().with_font_size(24).with_color(COR_TEXTO)),
        )
        .element(
            // Marca em texto estilizado: negrito itálico no azul de destaque.
            Paragraph::new("Vínculo")
                .aligned(Alignment::Right)
                .styled(
                    Style::new()
                        .bold()
                        .italic()
                        .with_font_size(14)
                        .with_color(COR_SECAO),
                )
                .padded(Margins::trbl(2.0, 0.0, 0.0, 0.0)),
        )
        .push()
        .map_err(|e| anyhow!("Falha ao montar o cabeçalho do PDF: {e}"))?;
    doc.push(topo);
    doc.push(Break::new(0.6));

    // Metadados: rótulo em negrito, valor em texto normal.
    let meta = |rotulo: &str, valor: &str| {
        let mut p = Paragraph::default();
        p.push_styled(format!("{rotulo}: "), estilo_corpo().bold());
        p.push_styled(valor, estilo_corpo());
        p.padded(Margins::trbl(0.4, 0.0, 0.4, 0.0))
    };
    doc.push(meta("Paciente", dados.paciente));
    doc.push(meta("Data", dados.data_sessao));

    // Linha do profissional apenas se houver dado a exibir.
    let profissional = dados.profissional.trim();
    let crp = dados.crp.trim();
    if !profissional.is_empty() || !crp.is_empty() {
        let valor = match (profissional.is_empty(), crp.is_empty()) {
            (false, false) => format!("{profissional} — {crp}"),
            (false, true) => profissional.to_string(),
            (true, false) => crp.to_string(),
            (true, true) => unreachable!(),
        };
        doc.push(meta("Profissional", &valor));
    }

    // Linha horizontal separando o cabeçalho do corpo.
    doc.push(LinhaHorizontal { cor: COR_LINHA }.padded(Margins::trbl(3.0, 0.0, 3.0, 0.0)));
    Ok(())
}

/// Interpreta o markdown leve, linha a linha, e adiciona os elementos ao corpo.
fn adicionar_corpo(doc: &mut Document, markdown: &str) {
    for linha in markdown.lines() {
        // Remove a marcação "**" (negrito markdown não é renderizado
        // arbitrariamente; apenas os rótulos conhecidos ganham negrito).
        let limpa = linha.replace("**", "");
        let aparada = limpa.trim();

        if aparada.is_empty() {
            // Linha vazia vira um pequeno respiro vertical.
            doc.push(Break::new(0.5));
            continue;
        }

        if let Some(texto) = aparada.strip_prefix("### ") {
            // Subtítulo de tema (nota: o genpdf usa tamanho de fonte inteiro,
            // então os 12.5pt pedidos viram 13pt).
            doc.push(
                Paragraph::new(texto.trim())
                    .styled(Style::new().bold().with_font_size(13).with_color(COR_SUBSECAO))
                    .padded(Margins::trbl(2.5, 0.0, 1.0, 0.0)),
            );
            continue;
        }

        // "## Título" — e, por segurança, "# Título" recebe o mesmo tratamento.
        if let Some(texto) = aparada
            .strip_prefix("## ")
            .or_else(|| aparada.strip_prefix("# "))
        {
            doc.push(
                Paragraph::new(texto.trim())
                    .styled(Style::new().bold().with_font_size(16).with_color(COR_SECAO))
                    .padded(Margins::trbl(4.5, 0.0, 1.5, 0.0)),
            );
            continue;
        }

        if let Some(texto) = aparada
            .strip_prefix("- ")
            .or_else(|| aparada.strip_prefix("* "))
        {
            let mut item = Paragraph::default();
            item.push_styled(texto.trim(), estilo_corpo());
            doc.push(
                BulletPoint::new(item)
                    .with_bullet("•")
                    .padded(Margins::trbl(0.4, 0.0, 0.4, 2.0)),
            );
            continue;
        }

        // Rótulos conhecidos: só o rótulo em negrito, no mesmo parágrafo.
        if let Some(rotulo) = ROTULOS.iter().find(|r| aparada.starts_with(**r)) {
            let resto = &aparada[rotulo.len()..];
            let mut p = Paragraph::default();
            p.push_styled(*rotulo, estilo_corpo().bold());
            if !resto.trim().is_empty() {
                p.push_styled(format!(" {}", resto.trim()), estilo_corpo());
            }
            doc.push(p.padded(Margins::trbl(0.8, 0.0, 0.8, 0.0)));
            continue;
        }

        // Parágrafo comum.
        let mut p = Paragraph::default();
        p.push_styled(aparada, estilo_corpo());
        doc.push(p.padded(Margins::trbl(0.8, 0.0, 0.8, 0.0)));
    }
}

/// Monta o documento completo (fontes, decorador, cabeçalho e corpo).
fn montar_documento(
    dados: &DadosAnotacao,
    fontes: FontFamily<FontData>,
    decorador: DecoradorRelatorio,
) -> Result<Document> {
    let mut doc = Document::new(fontes);
    doc.set_paper_size(PaperSize::A4);
    doc.set_title(format!("Anotação — {}", dados.paciente));
    doc.set_page_decorator(decorador);

    adicionar_cabecalho(&mut doc, dados)?;
    adicionar_corpo(&mut doc, dados.conteudo_markdown);
    Ok(doc)
}

/// Gera o PDF da anotação em `destino`.
///
/// A renderização é feita em duas passadas: a primeira (descartada) apenas
/// conta as páginas, e a segunda grava o arquivo com o rodapé
/// "Página X de Y" correto em todas as páginas.
pub fn gerar_pdf_anotacao(dados: &DadosAnotacao, destino: &Path) -> Result<()> {
    let fontes = carregar_fontes()?;

    // 1ª passada: renderiza para o "nada" só para contar as páginas.
    let contador = Arc::new(AtomicUsize::new(0));
    let rascunho = montar_documento(
        dados,
        fontes.clone(),
        DecoradorRelatorio {
            pagina: 0,
            total_paginas: None,
            contador: Some(Arc::clone(&contador)),
        },
    )?;
    rascunho
        .render(io::sink())
        .map_err(|e| anyhow!("Falha ao calcular a paginação do PDF: {e}"))?;
    let total_paginas = contador.load(Ordering::Relaxed).max(1);

    // 2ª passada: renderização definitiva, agora com o total de páginas.
    let definitivo = montar_documento(
        dados,
        fontes,
        DecoradorRelatorio {
            pagina: 0,
            total_paginas: Some(total_paginas),
            contador: None,
        },
    )?;
    let arquivo = std::fs::File::create(destino)
        .with_context(|| format!("Não foi possível criar o arquivo {}", destino.display()))?;
    definitivo
        .render(io::BufWriter::new(arquivo))
        .map_err(|e| anyhow!("Falha ao gravar o PDF em {}: {e}", destino.display()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Testes
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gera_pdf_de_exemplo() {
        let markdown = "\
## Temas trabalhados

### Sobrecarga e organização

Resumo: a paciente relatou **sobrecarga** no trabalho e dificuldade em delegar tarefas, com impacto no sono.

Pontos registrados:
- Acúmulo de responsabilidades após promoção recente
- Sensação de culpa ao estabelecer limites com colegas
* Episódios de insônia inicial nas noites de domingo

Leitura pela abordagem: o padrão sugere crenças de autoexigência elevada, com reforço de esquemas de desempenho.

## Intervenções e recursos utilizados

- Psicoeducação sobre higiene do sono
- Registro de pensamentos disfuncionais (RPD)
- Ensaio comportamental para comunicação assertiva

## Resposta e evolução observada

A paciente demonstrou boa adesão às técnicas propostas e conseguiu identificar gatilhos com mais autonomia do que nas sessões anteriores. Houve redução relatada da ruminação noturna.

## Combinados e próximos passos

- Manter o diário de sono durante a semana
- Trazer o RPD preenchido para a próxima sessão
- Avaliar a conversa com a liderança sobre redistribuição de tarefas";

        let dados = DadosAnotacao {
            paciente: "Maria Exemplo da Silva",
            data_sessao: "07/10/2026 às 16:00",
            profissional: "Dra. Ana Terapeuta",
            crp: "CRP 05/12345",
            conteudo_markdown: markdown,
        };

        let destino = std::env::temp_dir().join("vinculo_anotacao_teste.pdf");
        gerar_pdf_anotacao(&dados, &destino).expect("geração do PDF deveria funcionar");

        let metadados = std::fs::metadata(&destino).expect("o PDF deveria existir");
        assert!(
            metadados.len() > 1024,
            "o PDF deveria ter mais de 1KB, tem {} bytes",
            metadados.len()
        );
        println!("PDF de teste gerado em: {}", destino.display());
    }

    #[test]
    fn gera_pdf_com_varias_paginas() {
        // Conteúdo longo para forçar a quebra automática de página e
        // exercitar o rodapé "Página X de Y" em mais de uma página.
        let bloco = "## Temas trabalhados\n\n### Sobrecarga e organização\n\nResumo: parágrafo \
longo de exemplo para ocupar espaço vertical no documento e forçar a paginação automática do \
genpdf ao longo de várias páginas do relatório clínico.\n\nPontos registrados:\n- Primeiro item \
de exemplo\n- Segundo item de exemplo\n- Terceiro item de exemplo\n\n";
        let markdown = bloco.repeat(12);

        let dados = DadosAnotacao {
            paciente: "Paciente de Teste",
            data_sessao: "07/10/2026 às 16:00",
            profissional: "",
            crp: "",
            conteudo_markdown: &markdown,
        };

        let destino = std::env::temp_dir().join("vinculo_anotacao_teste_multipagina.pdf");
        gerar_pdf_anotacao(&dados, &destino).expect("geração do PDF multipágina deveria funcionar");
        assert!(std::fs::metadata(&destino).expect("o PDF deveria existir").len() > 1024);
    }
}
