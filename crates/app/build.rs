fn main() {
    // Força o estilo claro: a UI tem fundo claro fixo, então os widgets não
    // podem seguir o tema escuro do Windows (texto branco sobre fundo branco).
    let config = slint_build::CompilerConfiguration::new().with_style("fluent-light".into());
    slint_build::compile_with_config("ui/main.slint", config).expect("falha ao compilar a UI Slint");
}
