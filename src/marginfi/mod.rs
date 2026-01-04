mod types;
mod consts;
mod events;
mod macros;
mod wrapped_i80f48;

use std::rc::Rc;

use bytemuck::Pod;
use consts::*;
use events::*;
use wrapped_i80f48::*;

use solana_client::nonblocking::pubsub_client::PubsubClient;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_client::rpc_config::{CommitmentConfig, RpcTransactionLogsConfig, RpcTransactionLogsFilter};
use anchor_client::{Client, Cluster, Program};
use anchor_client::solana_sdk::signature::Keypair;
use solana_sdk::pubkey::Pubkey;
use tokio_stream::StreamExt;

use crate::consts::MARGINFI_PROGRAM_ID;
use crate::marginfi::types::{Bank, MarginfiAccount};

pub struct Marginfi {
  pubsub: PubsubClient,
  rpc_client: RpcClient,
  client: Client<Rc<Keypair>>,
  program: Program<Rc<Keypair>>
}

impl Marginfi {
  pub async fn new(http_url: String, ws_url: String) -> anyhow::Result<Self> {
    let pubsub = PubsubClient::new(ws_url).await?;
    let payer = Keypair::new();
    let rpc_client = RpcClient::new(http_url);
    let client = Client::new(Cluster::Mainnet, Rc::new(payer));
    let program = client.program(MARGINFI_PROGRAM_ID)?;
    
    anyhow::Ok(Self { pubsub, rpc_client, client, program })
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
            println!("  Account: {}", event.header.marginfi_account);
            println!("  Transaction: {}", signature);
            
            let sdk_pubkey = Pubkey::new_from_array(event.header.marginfi_account.to_bytes());
            let account: MarginfiAccount = match parse_account(&self.rpc_client, &sdk_pubkey).await {
              Ok(account) => account,
              Err(err) => {
                println!("error parsing account data: {err}");
                continue;
              },
            };
            println!("ACCOUNT DATA");
            println!("  Owner: {}", account.authority);
            println!("  Lended assets:");

            for balance in account.lending_account.get_active_balances_iter() {
              let sdk_pubkey = Pubkey::new_from_array(balance.bank_pk.to_bytes());
              let bank_account: Bank = match parse_account(&self.rpc_client, &sdk_pubkey).await {
                Ok(account) => account,
                Err(err) => {
                  println!("error parsing bank data: {err}");
                  continue;
                },
              };
              println!("    Mint: {}", bank_account.mint);
              println!("    Amount: {:?}", balance.asset_shares);
              println!("    Bank: {}", balance.bank_pk);
            }
            println!();

          }
        }
      }
    }

    anyhow::Ok(())
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

async fn parse_account<T: Pod>(
  rpc_client: &RpcClient,
  account_pubkey: &Pubkey,
) -> Result<T, Box<dyn std::error::Error>> {
  let account_data = rpc_client.get_account_data(account_pubkey).await?;
  
  let data_without_discriminator = &account_data[8..];
  
  let marginfi_account = bytemuck::try_from_bytes::<T>(data_without_discriminator)
      .map_err(|e| format!("failed to deserialize: {:?}", e))?;
  
  Ok(*marginfi_account)
}