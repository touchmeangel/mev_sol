use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use anchor_lang::prelude::{Clock, Pubkey};

use crate::{marginfi::types::{Bank, MarginfiAccount, OraclePriceFeedAdapter, OraclePriceFeedAdapterConfig}, utils::parse_account};

#[derive(Clone)]
pub struct MarginfiUserAccount {
  account: MarginfiAccount,
  banks: Vec<BankWithPriceFeed>,
}

impl MarginfiUserAccount {
  pub async fn from_pubkey(rpc_client: &RpcClient, account_pubkey: &Pubkey, clock: &Clock) -> anyhow::Result<Self> {
    let account_data = rpc_client.get_account(account_pubkey).await?.data;
    let account = parse_account::<MarginfiAccount>(&account_data)
      .map_err(|e| anyhow::anyhow!("invalid account data: {}", e))?;
    
    let bank_pubkeys: Vec<Pubkey> = account
      .lending_account
      .get_active_balances_iter()
      .map(|balance| balance.bank_pk)
      .collect();

    let bank_accounts = rpc_client.get_multiple_accounts(&bank_pubkeys).await?
      .into_iter()
      .collect::<Option<Vec<_>>>()
      .ok_or(anyhow::anyhow!("get_multiple_accounts failed to load all bank accounts"))?;

    let banks = bank_accounts
      .iter()
      .map(|account| parse_account::<Bank>(&account.data))
      .collect::<Result<Vec<_>, _>>()
      .map_err(|e| anyhow::anyhow!("invalid bank data: {}", e))?;

    let configs = OraclePriceFeedAdapterConfig::load_multiple_with_clock(rpc_client, &banks, clock).await?;
    let price_feeds = configs
      .into_iter()
      .map(|cfg| OraclePriceFeedAdapter::try_from_config(cfg))
      .collect::<Result<Vec<_>, _>>()?;

    anyhow::Ok(Self {
      account,
      banks: banks.into_iter().zip(price_feeds).map(|(bank, price_feed)| BankWithPriceFeed { bank, price_feed }).collect(),
    })
  } 

  pub fn account(&self) -> &MarginfiAccount {
    &self.account
  }
}

#[derive(Clone)]
pub struct BankWithPriceFeed {
  pub bank: Bank,
  pub price_feed: OraclePriceFeedAdapter,
}