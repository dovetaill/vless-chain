fn main() -> anyhow::Result<()> {
    if let Some(code) = chain_panel::task::command_fixture()? {
        std::process::exit(code);
    }
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if chain_panel::task::fixture(&args)? {
        return Ok(());
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run(args))
}
async fn run(args: Vec<String>) -> anyhow::Result<()> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        result = chain_panel::task::main(args) => result,
        _ = tokio::signal::ctrl_c() => anyhow::bail!("操作已中断"),
        _ = terminate.recv() => anyhow::bail!("收到终止信号"),
    }
}
