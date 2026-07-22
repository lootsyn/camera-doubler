#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match std::env::args().nth(1).as_deref() {
        None | Some("serve") => linux::run().await,
        Some("healthcheck") => linux::healthcheck().await,
        Some(command) => anyhow::bail!("unknown command {command}"),
    }
}

#[cfg(not(target_os = "linux"))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("the production Web Relay runtime requires Linux")
}
