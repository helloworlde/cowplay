use super::{Result, transport::Link, worker::Config};
use f50_core::tesla::{Client, ClimateCommand, ClimateState, Identity};
use std::time::Duration;
use tokio::sync::watch;

pub(super) async fn run(
    config: Config,
    mut stop: watch::Receiver<bool>,
    bus: &str,
    adapter: u16,
    command: Option<ClimateCommand>,
    authorized: impl Fn() + Send,
) -> Result<Option<ClimateState>> {
    let bytes = f50_server::settings::read(
        config
            .private_key
            .to_str()
            .ok_or("invalid Tesla identity path")?,
        4096,
    )?;
    let mut client = Client::new(&config.vin, Identity::from_pkcs8(&bytes)?)?;
    let mut link = Link::open_on(bus, &client.local_name(), stop.clone(), adapter).await?;
    let result = async {
        tokio::time::timeout(Duration::from_secs(4), async {
            link.send(&client.hello(), &mut stop).await?;
            loop {
                if client.accept_hello(&link.receive(&mut stop).await?)? {
                    return Ok::<_, Box<dyn std::error::Error + Send + Sync>>(());
                }
            }
        })
        .await??;
        authorized();
        let request = match command {
            Some(command) => client.climate_command(command)?,
            None => client.climate_query()?,
        };
        // Never resend a command after timeout, resync, missing ACK, or cancellation.
        link.send(&request, &mut stop).await?;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let packet = link.receive(&mut stop).await?;
                if command.is_some() {
                    if client.accept_climate_ack(&packet)? {
                        return Ok(None);
                    }
                } else if let Some(state) = client.accept_climate_response(&packet)? {
                    return Ok(Some(state));
                }
            }
        })
        .await?
    }
    .await;
    let cleanup = link.close().await;
    cleanup?;
    result
}
