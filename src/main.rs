mod config;
mod consts;
mod marginfi;
mod utils;

use config::Config;

use crate::marginfi::Marginfi;

#[tokio::main]
async fn main() {
  let result: anyhow::Result<()> = async move {
    let config = Config::open().await?;

    let marginfi = Marginfi::new(config.url, config.ws_url).await?;
    marginfi.listen_for_targets().await?;
    
    Ok(())
  }.await;

  if let Err(err) = result {
    eprintln!("Error: {err}");
    
    err.chain()
        .skip(1)
        .for_each(|cause| eprintln!("caused by:\n  {cause}"));
  }
}