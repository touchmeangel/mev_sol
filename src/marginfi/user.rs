use anyhow::Context;
use fixed::types::I80F48;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use anchor_lang::prelude::{Pubkey};

use crate::{marginfi::types::{Balance, BalanceSide, Bank, MarginfiAccount, OraclePriceFeedAdapter, OraclePriceFeedAdapterConfig, OraclePriceType, PriceAdapter, reconcile_emode_configs}, utils::parse_account};

#[derive(Clone)]
pub struct MarginfiUserAccount {
  account: MarginfiAccount,
  bank_accounts: Vec<BankAccount>,
}

impl MarginfiUserAccount {
  pub async fn from_pubkey(rpc_client: &RpcClient, account_pubkey: &Pubkey) -> anyhow::Result<Self> {
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

    let configs = OraclePriceFeedAdapterConfig::load_multiple(rpc_client, &banks).await?;
    let price_feeds = configs
      .into_iter()
      .map(|cfg| OraclePriceFeedAdapter::try_from_config(cfg))
      .collect::<Result<Vec<_>, _>>()?;

    let banks: Vec<BankAccount> = banks
      .into_iter()
      .zip(account
        .lending_account
        .get_active_balances_iter())
      .zip(price_feeds)
      .map(|((bank, balance), price_feed)| BankAccount { bank, price_feed, balance: *balance })
      .collect();

    let reconciled_emode_config = reconcile_emode_configs(
      banks
        .iter()
        .filter(|b| !b.balance.is_empty(BalanceSide::Liabilities))
        .map(|b| b.bank.emode.emode_config),
    );

    anyhow::Ok(Self {
      account,
      bank_accounts: banks,
    })
  } 

  pub fn account(&self) -> &MarginfiAccount {
    &self.account
  }
  
  /// returns lended value in usd
  pub fn asset_value(&self) -> anyhow::Result<I80F48> {
    let total_asset_value: I80F48 = self.bank_accounts.iter()
      .try_fold(I80F48::ZERO, |acc, bank_account| {
        let asset_value = bank_account.asset_value()?;
    
        anyhow::Ok(acc + asset_value)
      })?;

    anyhow::Ok(total_asset_value)
  }

  /// returns borrowed value in usd
  pub fn liability_value(&self) -> anyhow::Result<I80F48> {
    let total_liability_value: I80F48 = self.bank_accounts.iter()
      .try_fold(I80F48::ZERO, |acc, bank_account| {
        let liability_value = bank_account.liability_value()?;

        anyhow::Ok(acc + liability_value)
      })?;

    anyhow::Ok(total_liability_value)
  }

  pub fn maintenance(&self) -> anyhow::Result<I80F48> {
    let mut total_asset_value: I80F48 = I80F48::ZERO;
    let mut total_liability_value: I80F48 = I80F48::ZERO;
    for bank_account in &self.bank_accounts {
      let asset_value = bank_account.asset_value()?;
      let liability_value = bank_account.liability_value()?;

      // If an emode entry exists for this bank's emode tag in the reconciled config of
      // all borrowing banks, use its weight, otherwise use the weight designated on the
      // collateral bank itself. If the bank's weight is higher, always use that weight.
      let asset_weight: I80F48 = bank_account.bank.config.asset_weight_maint.into();
      let liability_weight: I80F48 = bank_account.bank.config.liability_weight_maint.into();
      // bank.bank.emode.emode_config.find_with_tag(tag)

      total_asset_value += asset_value.checked_mul(asset_weight)
        .context("asset maintenance value calculation failed")?;
      total_liability_value += liability_value.checked_mul(liability_weight)
        .context("liability maintenance value calculation failed")?;
    }

    println!("a: {}, l: {}", total_asset_value, total_liability_value);
    anyhow::Ok(total_asset_value - total_liability_value)
  }
}

#[derive(Clone)]
pub struct BankAccount {
  pub bank: Bank,
  pub price_feed: OraclePriceFeedAdapter,
  pub balance: Balance
}

impl BankAccount {
  pub fn asset_value(&self) -> anyhow::Result<I80F48> {
    if self.balance.is_empty(BalanceSide::Liabilities) {
      return anyhow::Ok(I80F48::ZERO);
    }
    let price = self.price_feed.get_price_of_type(
      OraclePriceType::RealTime,
      Some(super::types::PriceBias::Low),
      self.bank.config.oracle_max_confidence
    )?;

    let asset = self.bank.get_asset_amount(self.balance.asset_shares.into())
      .context("asset shares calculation failed")?;

    let asset_value_with_decimals = asset.checked_mul(price)
      .context("asset with decimals value calculation failed")?;

    let asset_value = self.bank.get_display_asset(asset_value_with_decimals)
      .context("asset value calculation failed")?;

    anyhow::Ok(asset_value)
  }

  pub fn liability_value(&self) -> anyhow::Result<I80F48> {
    if self.balance.is_empty(BalanceSide::Liabilities) {
      return anyhow::Ok(I80F48::ZERO);
    }
    let price = self.price_feed.get_price_of_type(
      OraclePriceType::RealTime,
      Some(super::types::PriceBias::Low),
      self.bank.config.oracle_max_confidence
    )?;

    let liability = self.bank.get_asset_amount(self.balance.liability_shares.into())
      .context("liability shares calculation failed")?;

    let liability_value_with_decimals = liability.checked_mul(price)
      .context("liability with decimals value calculation failed")?;

    let liability_value = self.bank.get_display_asset(liability_value_with_decimals)
      .context("liability value calculation failed")?;

    anyhow::Ok(liability_value)
  }
}