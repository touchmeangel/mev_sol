mod consts;
mod events;
mod macros;
mod wrapped_i80f48;

use consts::*;
use events::*;
use wrapped_i80f48::*;

use solana_client::nonblocking::pubsub_client::PubsubClient;
use solana_client::rpc_config::{CommitmentConfig, RpcTransactionLogsConfig, RpcTransactionLogsFilter};
use tokio_stream::StreamExt;

use crate::consts::MARGINFI_PROGRAM_ID;

pub struct Marginfi {
  pubsub: PubsubClient
}

impl Marginfi {
  pub async fn new(ws_url: String) -> anyhow::Result<Self> {
    let pubsub = PubsubClient::new(ws_url).await?;
    
    anyhow::Ok(Self { pubsub })
  }

  pub async fn listen(&self) -> anyhow::Result<()> {
    let (mut logs, _unsub) = self.pubsub
        .logs_subscribe(
            RpcTransactionLogsFilter::Mentions(vec![MARGINFI_PROGRAM_ID.to_string()]),
            RpcTransactionLogsConfig {
              commitment: Some(CommitmentConfig::confirmed()),
            },
        )
        .await?;

    println!("✅ Connected! Listening for liquidation events...\n");

    while let Some(response) = logs.next().await {
      let signature = &response.value.signature;
      let err = response.value.err.is_some();
      
      if err {
        continue;
      }

      for log in &response.value.logs {
        if let Some(event_data) = log.strip_prefix("Program data: ") {
          if let Ok(event) = parse_anchor_event::<HealthPulseEvent>(event_data) {
            println!("HEALTH PULSE!");
            println!("  Account: {}", event.account);
            println!("  Transaction: {}", signature);
            println!();
          }
        }
      }
    }

    anyhow::Ok(())
  }

  // lending_account_liquidate // legacy
  // start_liquidation, end_liquidation // receivership
  // lending_account_pulse_health

  // Example:
  // ```
  // Token A: Asset Weight Initial = 90%, Asset Weight Maintenance = 95%
  // Token B: Liability Weight Initial = 110%, Liability Weight Maintenance = 105%
  
  // Susie deposits $100 of Token A
  //     Susie's deposit is worth: 100 * .9 = $90 for Initial (borrowing) purposes.
  
  // Susie wants to borrow as much B as possible
  //     The max borrow of B is: 90/1.1 = $81.82
  
  // For Maintenance purposes, Susie has 100 * .95 - 81.82 * 1.05 = $9.089 left in liquidation buffer.
  // ````
  
  // Now let's imagine Asset A drops in value by 5%, and B stays the same:
  
  // ```
  // Susie's deposit in A is now worth $95
  
  // For Maintenance purposes, Susie has 95 * .95 - 81.82 * 1.05 = $4.339 left in liquidation buffer.
  // ````
  
  // Now let's imagine Asset A drops in value by 10% (net), and B stays the same:
  
  // ```
  // Susie's deposit in A is now worth $90
  
  // For Maintenance purposes, Susie has 90 * .95 - 81.82 * 1.05 = -$0.411, Susie can be liquidated!
  // ````
}

fn parse_anchor_event<T: anchor_lang::AnchorDeserialize>(data: &str) -> anyhow::Result<T> {
  use base64::{Engine as _, engine::general_purpose};
  let decoded = general_purpose::STANDARD.decode(data)?;
  let event_data = &decoded[8..];
  Ok(T::deserialize(&mut &event_data[..])?)
}