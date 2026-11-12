//! Temporary diagnostic client, not a replacement CowPlay service or a pairing tool.
use std::time::Duration;
#[tokio::main]
async fn main() -> cowplay::board::Result<()> {
    let seconds: u64 = std::env::args()
        .nth(1)
        .ok_or("usage: tesla-ble-scan SECONDS")?
        .parse()?;
    let (stop, cancelled) = tokio::sync::watch::channel(false);
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::spawn(async move {
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
        let _ = stop.send(true);
    });
    let report =
        cowplay::tesla_ble::scan(cowplay::board::BUS, Duration::from_secs(seconds), cancelled)
            .await?;
    println!("{}", serde_json::to_string(&report)?);
    Ok(())
}
