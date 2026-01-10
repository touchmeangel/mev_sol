use std::collections::HashMap;

use anyhow::Context;
use fixed::types::I80F48;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use anchor_lang::prelude::{Clock, Pubkey, sysvar::clock};

use crate::{marginfi::types::{Bank, MarginfiAccount, OraclePriceFeedAdapter, OraclePriceFeedAdapterConfig, OraclePriceType, PriceAdapter}, utils::parse_account};

#[derive(Clone)]
pub struct MarginfiUserAccount {
  account: MarginfiAccount,
  banks: HashMap<Pubkey, BankWithPriceFeed>,
}

impl MarginfiUserAccount {
  pub async fn from_pubkey(rpc_client: &RpcClient, account_pubkey: Pubkey) -> anyhow::Result<Self> {
    let pre_request_data = rpc_client.get_multiple_accounts(&[account_pubkey, clock::ID])
      .await?
      .into_iter()
      .collect::<Option<Vec<_>>>()
      .ok_or(anyhow::anyhow!("get_multiple_accounts failed"))?;

    let account = parse_account::<MarginfiAccount>(&pre_request_data[0].data)
      .map_err(|e| anyhow::anyhow!("invalid account data: {}", e))?;
    let clock: Clock = bincode::deserialize(&pre_request_data[1].data)?;
    
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

    let configs = OraclePriceFeedAdapterConfig::load_multiple_with_clock(rpc_client, &banks, &clock).await?;
    let price_feeds = configs
      .into_iter()
      .map(|cfg| OraclePriceFeedAdapter::try_from_config(cfg))
      .collect::<Result<Vec<_>, _>>()?;

    let banks = banks
      .into_iter()
      .zip(bank_pubkeys)
      .zip(price_feeds)
      .map(|((bank, bank_pk), price_feed)| (bank_pk, BankWithPriceFeed { bank, price_feed }))
      .collect();

    anyhow::Ok(Self {
      account,
      banks,
    })
  } 

  pub fn account(&self) -> &MarginfiAccount {
    &self.account
  }
  
  /// returns lended value in usd
  pub fn asset_value(&self) -> anyhow::Result<I80F48> {
    let total_asset_value: I80F48 = self.account.lending_account.get_active_balances_iter()
      .try_fold(I80F48::ZERO, |acc, balance| {
        let bank = self.banks.get(&balance.bank_pk)
          .ok_or_else(|| anyhow::anyhow!("Bank not found"))?;
    
        let price = bank.price_feed.get_price_of_type(
          OraclePriceType::RealTime,
          Some(super::types::PriceBias::Low),
          bank.bank.config.oracle_max_confidence
        )?;
    
        let asset = bank.bank.get_asset_amount(balance.asset_shares.into())
          .context("asset shares calculation failed")?;
    
        let asset_value_with_decimals = asset.checked_mul(price)
          .context("asset with decimals value calculation failed")?;

        let asset_value = bank.bank.get_display_asset(asset_value_with_decimals)
          .context("asset value calculation failed")?;
    
        anyhow::Ok(acc + asset_value)
      })?;

    anyhow::Ok(total_asset_value)
  }

  /// returns borrowed value in usd
  pub fn liability_value(&self) -> anyhow::Result<I80F48> {
    let total_liability_value: I80F48 = self.account.lending_account.get_active_balances_iter()
      .try_fold(I80F48::ZERO, |acc, balance| {
        let bank = self.banks.get(&balance.bank_pk)
          .ok_or_else(|| anyhow::anyhow!("Bank not found"))?;
    
        let price = bank.price_feed.get_price_of_type(
          OraclePriceType::RealTime,
          Some(super::types::PriceBias::Low),
          bank.bank.config.oracle_max_confidence
        )?;
    
        let liability = bank.bank.get_asset_amount(balance.liability_shares.into())
          .context("liability shares calculation failed")?;
    
        let liability_value_with_decimals = liability.checked_mul(price)
          .context("liability with decimals value calculation failed")?;

        let liability_value = bank.bank.get_display_asset(liability_value_with_decimals)
          .context("liability value calculation failed")?;

        anyhow::Ok(acc + liability_value)
      })?;

    anyhow::Ok(total_liability_value)
  }

  pub fn get_bank(&self, pubkey: &Pubkey) -> Option<&Bank> {
    self.banks.get(pubkey).map(|b| &b.bank)
  }
}

#[derive(Clone)]
pub struct BankWithPriceFeed {
  pub bank: Bank,
  pub price_feed: OraclePriceFeedAdapter,
}