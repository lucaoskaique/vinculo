fn main() {
    // Força o estilo claro: a UI tem fundo claro fixo, então os widgets não
    // podem seguir o tema escuro do Windows (texto branco sobre fundo branco).
    let config = slint_build::CompilerConfiguration::new().with_style("fluent-light".into());
    slint_build::compile_with_config("ui/main.slint", config)
        .expect("falha ao compilar a UI Slint");

    // Ícone do executável (barra de tarefas/Explorer) no Windows.
    #[cfg(windows)]
    {
        let mut recurso = winres::WindowsResource::new();
        recurso.set_icon("assets/vinculo.ico");
        recurso
            .compile()
            .expect("falha ao embutir o ícone do executável");
    }
}
