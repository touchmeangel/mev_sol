mod instructions;
mod user;
mod types;
mod consts;
mod errors;
mod events;
mod macros;
mod prelude;
mod wrapped_i80f48;

use anchor_lang::prelude::sysvar::clock;
use instructions::*;
use consts::*;
pub use errors::*;
use events::*;
use solana_account_decoder::UiAccountEncoding;
use solana_transaction_status_client_types::UiTransactionEncoding;
use wrapped_i80f48::*;
use user::*;

use std::rc::Rc;

use anchor_lang::prelude::{Clock, Pubkey};
use anchor_client::solana_sdk::commitment_config::CommitmentConfig;
use solana_rpc_client_types::config::{RpcSimulateTransactionAccountsConfig, RpcSimulateTransactionConfig, RpcTransactionLogsConfig, RpcTransactionLogsFilter};
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_pubsub_client::nonblocking::pubsub_client::PubsubClient;
use anchor_client::{Client, Cluster, Program};
use anchor_client::solana_sdk::signature::Keypair;
use tokio_stream::StreamExt;
use std::time::Instant;

use crate::consts::MARGINFI_PROGRAM_ID;
use crate::marginfi::types::{Bank, MarginfiAccount, OraclePriceFeedAdapter, OraclePriceFeedAdapterConfig, PriceAdapter};
use crate::utils::parse_account;

pub struct Marginfi {
  pubsub: PubsubClient,
  rpc_client: RpcClient,
  client: Client<Rc<Keypair>>,
  program: Program<Rc<Keypair>>,
  clock: Clock
}

impl Marginfi {
  pub async fn new(http_url: String, ws_url: String) -> anyhow::Result<Self> {
    let pubsub = PubsubClient::new(&ws_url).await?;
    let payer = Rc::new(Keypair::new());
    let client = Client::new(Cluster::Custom(http_url, ws_url), payer);
    let program = client.program(MARGINFI_PROGRAM_ID)?;
    let rpc_client = program.rpc();

    let clock_data = rpc_client.get_account_data(&clock::ID).await?;
    let clock: Clock = bincode::deserialize(&clock_data)?;

    anyhow::Ok(Self { pubsub, rpc_client, client, program, clock })
  }

  pub async fn listen_for_targets(&self) -> anyhow::Result<()> {
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
          if let Ok(event) = parse_anchor_event::<LendingAccountWithdrawEvent>(event_data) {
            println!("WITHDRAW!");
            println!("  Transaction: {}", signature);
            
            self.handle_account(&event.header.marginfi_account).await?;
            println!();
          }
        }
      }
    }

    anyhow::Ok(())
  }

  async fn handle_account(&self, account_pubkey: &anchor_lang::prelude::Pubkey) -> anyhow::Result<()> {
    let account = MarginfiUserAccount::from_pubkey(&self.rpc_client, account_pubkey, &self.clock).await?;
    let marginfi_account = account.account();
    println!("ACCOUNT DATA");
    println!("  Owner: {}", marginfi_account.authority);
    println!("  Lended assets ({:?}$):", marginfi_account.health_cache.asset_value);

    anyhow::Ok(())
  }

  async fn lending_account_pulse_health(&self, account_pubkey: Pubkey) -> anyhow::Result<HealthCache> {
    let tx = self.program.request()
      .accounts(PulseHealthAccounts { marginfi_account: account_pubkey })
      .args(PulseHealth)
      .transaction()?;

    let config = RpcSimulateTransactionConfig {
      sig_verify: false,
      replace_recent_blockhash: true,
      commitment: Some(CommitmentConfig::processed()),
      encoding: Some(UiTransactionEncoding::Base64),
      accounts: Some(RpcSimulateTransactionAccountsConfig {
        encoding: Some(UiAccountEncoding::Base64),
        addresses: vec![],
      }),
      min_context_slot: None,
      inner_instructions: true,
    };
    let simulation_result = self.rpc_client.simulate_transaction_with_config(&tx, config).await?;
    if let Some(err) = simulation_result.value.err {
      anyhow::bail!("HealthPulseEvent simulation failed with: {}", err)
    }

    if let Some(logs) = simulation_result.value.logs {
      for log in logs {
        if let Some(event_data) = log.strip_prefix("Program data: ") {
          println!("Program data: {}", event_data);
          if let Ok(event) = parse_anchor_event::<HealthPulseEvent>(event_data) {
            return anyhow::Ok(event.health_cache);
          }
        }
      }
    }

    anyhow::bail!("HealthPulseEvent not found after simulation")
  }

    // /// Sum of all liability shares held by all borrowers in this bank.
    // /// * Uses `mint_decimals`
    // pub total_liability_shares: WrappedI80F48,
    // /// Sum of all asset shares held by all depositors in this bank.
    // /// * Uses `mint_decimals`
    // /// * For Kamino banks, this is the quantity of collateral tokens (NOT liquidity tokens) in the
    // ///   bank, and also uses `mint_decimals`, though the mint itself will always show (6) decimals
    // ///   exactly (i.e Kamino ignores this and treats it as if it was using `mint_decimals`)
    // pub total_asset_shares: WrappedI80F48,

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