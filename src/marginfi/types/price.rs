use super::super::consts::{
  MIN_PYTH_PUSH_VERIFICATION_LEVEL, NATIVE_STAKE_ID, PYTH_ID, SPL_SINGLE_POOL_ID,
  SWITCHBOARD_PULL_ID,
};
use anchor_lang::prelude::*;
use anchor_client::solana_sdk::{borsh::try_from_slice_unchecked, stake::state::StakeStateV2};
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use crate::utils::parse_account;
use crate::{check, check_eq, debug, live, math_error};
use super::super::prelude::*;
use anchor_spl::token::Mint;
use enum_dispatch::enum_dispatch;
use fixed::types::I80F48;
use super::kamino_mocks_state::{adjust_i128, adjust_i64, adjust_u64, MinimalReserve};
use super::super::consts::{CONF_INTERVAL_MULTIPLE, EXP_10_I80F48, MAX_CONF_INTERVAL, STD_DEV_MULTIPLE, U32_MAX, U32_MAX_DIV_10};
use super::{Bank, BankConfig, OracleSetup};
use pyth_solana_receiver_sdk::price_update::{self, FeedId, PriceUpdateV2};
use pyth_solana_receiver_sdk::PYTH_PUSH_ORACLE_ID;
use std::{cell::Ref, cmp::min};
use switchboard_on_demand::{
  CurrentResult, Discriminator, PullFeedAccountData, SPL_TOKEN_PROGRAM_ID,
};
#[derive(Copy, Clone, Debug)]
pub enum PriceBias {
  Low,
  High,
}

#[derive(Copy, Clone, Debug)]
pub enum OraclePriceType {
  /// Time weighted price
  /// EMA for PythEma
  TimeWeighted,
  /// Real time price
  RealTime,
}

#[enum_dispatch]
pub trait PriceAdapter {
  fn get_price_of_type(
      &self,
      oracle_price_type: OraclePriceType,
      bias: Option<PriceBias>,
      oracle_max_confidence: u32,
  ) -> MarginfiResult<I80F48>;

  fn get_price_of_type_ignore_conf(
      &self,
      t: OraclePriceType,
      b: Option<PriceBias>,
  ) -> MarginfiResult<I80F48> {
      self.get_price_of_type(t, b, u32::MAX)
  }
}

#[error_code]
pub enum OraclePriceFeedAdapterConfigError {
  #[msg("RPC error occurred")]
  RpcError,
}

async fn get_account(
  client: &RpcClient,
  key: &Pubkey,
) -> anyhow::Result<solana_account::Account> {
  client
    .get_account(key).await
    .map_err(|e| anyhow::anyhow!(OraclePriceFeedAdapterConfigError::RpcError).context(e))
}


#[derive(Debug)]
pub enum OracleAccounts {
  None,
  PythPush { price: solana_account::Account },
  SwitchboardPull { oracle: solana_account::Account },
  StakedWithPythPush {
    price: solana_account::Account,
    lst_mint: Mint,
    stake_state: solana_account::Account,
  },
  KaminoPythPush {
    price: solana_account::Account,
    reserve: solana_account::Account,
  },
  KaminoSwitchboardPull {
    oracle: solana_account::Account,
    reserve: solana_account::Account,
  },
}

pub struct OraclePriceFeedAdapterConfig<'info> {
  bank: &'info Bank,
  accounts: OracleAccounts,
  clock: &'info Clock,
  max_age: u64
}

impl<'info> OraclePriceFeedAdapterConfig<'info> {
  pub async fn load_with_clock_and_max_age(
    client: &RpcClient,
    bank: &'info Bank,
    clock: &'info Clock,
    max_age: u64
  ) -> anyhow::Result<Self> {
    let bank_config = &bank.config;

    let accounts = match bank_config.oracle_setup {
      OracleSetup::None => {
        return Err(anyhow::anyhow!(MarginfiError::OracleNotSetup));
      }
      OracleSetup::PythLegacy => {
        return Err(anyhow::anyhow!(ErrorCode::Deprecated));
      }
      OracleSetup::SwitchboardV2 => {
        return Err(anyhow::anyhow!(ErrorCode::Deprecated));
      }
      OracleSetup::PythPushOracle => {
        let price = get_account(client, &bank_config.oracle_keys[0]).await?;
        OracleAccounts::PythPush { price }
      }
      OracleSetup::SwitchboardPull => {
        let oracle = get_account(client, &bank_config.oracle_keys[0]).await?;
        OracleAccounts::SwitchboardPull { oracle }
      }
      OracleSetup::StakedWithPythPush => {
        let price = get_account(client, &bank_config.oracle_keys[0]).await?;
        let lst_mint_account = get_account(client, &bank_config.oracle_keys[1]).await?;
        let stake_state = get_account(client, &bank_config.oracle_keys[2]).await?;

        let lst_mint = Mint::try_deserialize(&mut (&lst_mint_account.data as &[u8]))?;
        OracleAccounts::StakedWithPythPush {
          price,
          lst_mint,
          stake_state,
        }
      }
      OracleSetup::KaminoPythPush => {
        let price = get_account(client, &bank_config.oracle_keys[0]).await?;
        let reserve = get_account(client, &bank_config.oracle_keys[1]).await?;
        OracleAccounts::KaminoPythPush { price, reserve }
      }
      OracleSetup::KaminoSwitchboardPull => {
        let oracle = get_account(client, &bank_config.oracle_keys[0]).await?;
        let reserve = get_account(client, &bank_config.oracle_keys[1]).await?;
        OracleAccounts::KaminoSwitchboardPull { oracle, reserve }
      }
      OracleSetup::Fixed => OracleAccounts::None,
    };

    Ok(Self { bank, accounts, clock, max_age })
  }

  pub async fn load_with_clock(
    client: &RpcClient,
    bank: &'info Bank,
    clock: &'info Clock
  ) -> anyhow::Result<Self> {
    Self::load_with_clock_and_max_age(client, bank, clock, bank.config.get_oracle_max_age()).await
  }
}

#[enum_dispatch(PriceAdapter)]
#[derive(Clone)]
pub enum OraclePriceFeedAdapter {
  PythPushOracle(PythPushOraclePriceFeed),
  SwitchboardPull(SwitchboardPullPriceFeed),
  Fixed(FixedPriceFeed),
}

impl OraclePriceFeedAdapter {
  pub fn try_from_config<'info>(config: OraclePriceFeedAdapterConfig<'info>) -> MarginfiResult<Self> {
      match config.accounts {
          OracleAccounts::None => {
              let price: I80F48 = config.bank.config.fixed_price.into();
              if price < I80F48::ZERO {
                  return Err(MarginfiError::FixedOraclePriceNegative.into());
              }
              Ok(OraclePriceFeedAdapter::Fixed(FixedPriceFeed { price }))
          }
          OracleAccounts::PythPush { price } => {
              let feed = PythPushOraclePriceFeed::load_checked(&price, config.clock, config.max_age)?;
              Ok(OraclePriceFeedAdapter::PythPushOracle(feed))
          }
          OracleAccounts::SwitchboardPull { oracle } => {
              let feed = SwitchboardPullPriceFeed::load_checked(
                &oracle, config.clock.unix_timestamp, config.max_age
              )?;
              Ok(OraclePriceFeedAdapter::SwitchboardPull(feed))
          }
          OracleAccounts::StakedWithPythPush { price, lst_mint, stake_state } => {
              // Deserialize stake state and compute adjusted price
              let stake_state = try_from_slice_unchecked::<StakeStateV2>(&stake_state.data)?;
              let (_, stake) = match stake_state {
                  StakeStateV2::Stake(_, stake, _) => ((), stake),
                  _ => return Err(ErrorCode::Deprecated.into()), // not supported
              };

              let sol_pool_balance = stake.delegation.stake;
              let lamports_per_sol: u64 = 1_000_000_000;
              let sol_pool_adjusted_balance =
                  sol_pool_balance.checked_sub(lamports_per_sol).ok_or_else(math_error!())?;

              let mut feed = PythPushOraclePriceFeed::load_checked(&price, config.clock, config.max_age)?;
              let lst_supply = lst_mint.supply;
              if lst_supply == 0 {
                  return Err(MarginfiError::ZeroSupplyInStakePool.into());
              }

              // Adjust price & EMA
              feed.price.price = ((feed.price.price as i128)
                  .checked_mul(sol_pool_adjusted_balance as i128).ok_or_else(math_error!())?
                  .checked_div(lst_supply as i128).ok_or_else(math_error!())?)
                  .try_into().unwrap();
              feed.ema_price.price = ((feed.ema_price.price as i128)
                  .checked_mul(sol_pool_adjusted_balance as i128).ok_or_else(math_error!())?
                  .checked_div(lst_supply as i128).ok_or_else(math_error!())?)
                  .try_into().unwrap();

              Ok(OraclePriceFeedAdapter::PythPushOracle(feed))
          }
          OracleAccounts::KaminoPythPush { price, reserve } => {
              let mut price_feed = PythPushOraclePriceFeed::load_checked(&price, config.clock, config.max_age)?;
              let (total_liq, total_col) = parse_account::<MinimalReserve>(&reserve.data)
                  .map_err(|_| ErrorCode::AccountDidNotDeserialize)?
                  .scaled_supplies()?;
              if total_col > I80F48::ZERO {
                  let ratio = total_liq / total_col;
                  price_feed.price.price = adjust_i64(price_feed.price.price, ratio)?;
                  price_feed.ema_price.price = adjust_i64(price_feed.ema_price.price, ratio)?;
                  price_feed.price.conf = adjust_u64(price_feed.price.conf, ratio)?;
                  price_feed.ema_price.conf = adjust_u64(price_feed.ema_price.conf, ratio)?;
              }
              Ok(OraclePriceFeedAdapter::PythPushOracle(price_feed))
          }
          OracleAccounts::KaminoSwitchboardPull { oracle, reserve } => {
              let mut price_feed =
                  SwitchboardPullPriceFeed::load_checked(&oracle, config.clock.unix_timestamp, config.max_age)?;
              let (total_liq, total_col) = parse_account::<MinimalReserve>(&reserve.data)
                  .map_err(|_| ErrorCode::AccountDidNotDeserialize)?
                  .scaled_supplies()?;
              if total_col > I80F48::ZERO {
                  let ratio = total_liq / total_col;
                  price_feed.feed.result.value =
                      adjust_i128(price_feed.feed.result.value, ratio)?;
                  price_feed.feed.result.std_dev =
                      adjust_i128(price_feed.feed.result.std_dev, ratio)?;
              }
              Ok(OraclePriceFeedAdapter::SwitchboardPull(price_feed))
          }
      }
  }
}

#[derive(Copy, Clone, Debug)]
pub struct FixedPriceFeed {
  pub price: I80F48,
}

impl PriceAdapter for FixedPriceFeed {
  fn get_price_of_type(
      &self,
      _oracle_price_type: OraclePriceType,
      _bias: Option<PriceBias>,
      _oracle_max_confidence: u32,
  ) -> MarginfiResult<I80F48> {
      Ok(self.price)
  }
}

#[derive(Clone, Debug)]
pub struct SwitchboardPullPriceFeed {
  pub feed: Box<LitePullFeedAccountData>,
}

impl SwitchboardPullPriceFeed {
  pub fn load_checked(
      account: &solana_account::Account,
      current_timestamp: i64,
      max_age: u64,
  ) -> MarginfiResult<Self> {
      let account_data = &account.data;

      let feed: PullFeedAccountData = parse_swb_ignore_alignment(account_data)?;
      let lite_feed = LitePullFeedAccountData::from(&feed);
      // TODO restore when swb fixes alignment issue in crate.
      // let feed = PullFeedAccountData::parse(ai_data)
      //     .map_err(|_| MarginfiError::SwitchboardInvalidAccount)?;

      // Check staleness
      let last_updated = feed.last_update_timestamp;
      if current_timestamp.saturating_sub(last_updated) > max_age as i64 {
          return err!(MarginfiError::SwitchboardStalePrice);
      }

      Ok(Self {
          feed: Box::new(lite_feed),
      })
  }

  fn check_ais(account: &solana_account::Account) -> MarginfiResult {
      let account_data = &account.data;

      let _feed = parse_swb_ignore_alignment(account_data)?;
      // TODO restore when swb fixes alignment issue in crate.
      // PullFeedAccountData::parse(ai_data)
      //     .map_err(|_| MarginfiError::SwitchboardInvalidAccount)?;

      Ok(())
  }

  fn get_price(&self) -> MarginfiResult<I80F48> {
      let sw_result = self.feed.result;
      // Note: Pull oracles support mean (result.mean) or median (result.value)
      let price: I80F48 = I80F48::from_num(sw_result.value)
          .checked_div(EXP_10_I80F48[switchboard_on_demand::PRECISION as usize])
          .ok_or_else(math_error!())?;
      Ok(price)
  }

  fn get_confidence_interval(&self, oracle_max_confidence: u32) -> MarginfiResult<I80F48> {
      let conf_interval: I80F48 = I80F48::from_num(self.feed.result.std_dev)
          .checked_div(EXP_10_I80F48[switchboard_on_demand::PRECISION as usize])
          .ok_or_else(math_error!())?
          .checked_mul(STD_DEV_MULTIPLE)
          .ok_or_else(math_error!())?;

      let price = self.get_price()?;

      // Fail the price fetch if confidence > price * oracle_max_confidence
      let oracle_max_confidence = if oracle_max_confidence > 0 {
          I80F48::from_num(oracle_max_confidence)
      } else {
          // The default max confidence is 10%
          U32_MAX_DIV_10
      };
      let max_conf = price
          .checked_mul(oracle_max_confidence)
          .ok_or_else(math_error!())?
          .checked_div(U32_MAX)
          .ok_or_else(math_error!())?;
      if conf_interval > max_conf {
          let conf_interval = conf_interval.to_num::<f64>();
          let max_conf = max_conf.to_num::<f64>();
          msg!("conf was {:?}, but max is {:?}", conf_interval, max_conf);
          return err!(MarginfiError::OracleMaxConfidenceExceeded);
      }

      // Clamp confidence to 5% of the price regardless
      let max_conf_interval = price
          .checked_mul(MAX_CONF_INTERVAL)
          .ok_or_else(math_error!())?;

      assert!(
          max_conf_interval >= I80F48::ZERO,
          "Negative max confidence interval"
      );

      assert!(
          conf_interval >= I80F48::ZERO,
          "Negative confidence interval"
      );

      Ok(min(conf_interval, max_conf_interval))
  }
}

impl PriceAdapter for SwitchboardPullPriceFeed {
  fn get_price_of_type(
      &self,
      _price_type: OraclePriceType,
      bias: Option<PriceBias>,
      oracle_max_confidence: u32,
  ) -> MarginfiResult<I80F48> {
      let price = self.get_price()?;

      match bias {
          Some(price_bias) => {
              let confidence_interval = self.get_confidence_interval(oracle_max_confidence)?;

              match price_bias {
                  PriceBias::Low => Ok(price
                      .checked_sub(confidence_interval)
                      .ok_or_else(math_error!())?),
                  PriceBias::High => Ok(price
                      .checked_add(confidence_interval)
                      .ok_or_else(math_error!())?),
              }
          }
          None => Ok(price),
      }
  }
}

// TODO remove when swb fixes the alignment issue in their crate
// (TargetAlignmentGreaterAndInputNotAligned) when bytemuck::from_bytes executes on any local system
// (including bpf next-test) where the struct is "properly" aligned 16
/// The same as PullFeedAccountData::parse but completely ignores input alignment.
pub fn parse_swb_ignore_alignment(data: &[u8]) -> MarginfiResult<PullFeedAccountData> {
  if data.len() < 8 {
      return err!(MarginfiError::SwitchboardInvalidAccount);
  }

  if data[..8] != PullFeedAccountData::DISCRIMINATOR[..8] {
      return err!(MarginfiError::SwitchboardInvalidAccount);
  }

  let feed = bytemuck::try_pod_read_unaligned::<PullFeedAccountData>(
      &data[8..8 + std::mem::size_of::<PullFeedAccountData>()],
  )
  .map_err(|_| MarginfiError::SwitchboardInvalidAccount)?;

  Ok(feed)
}

pub fn load_price_update_v2_checked(account: &solana_account::Account) -> MarginfiResult<PriceUpdateV2> {
  let price_feed_data = &account.data;
  let discriminator = &price_feed_data[0..8];
  let expected_discrim = <PriceUpdateV2 as anchor_lang::Discriminator>::DISCRIMINATOR;

  check_eq!(
      discriminator,
      expected_discrim,
      MarginfiError::PythPushInvalidAccount
  );

  Ok(PriceUpdateV2::deserialize(
      &mut &price_feed_data[8..],
  )?)
}

#[derive(Clone, Debug)]
pub struct PythPushOraclePriceFeed {
  ema_price: Box<pyth_solana_receiver_sdk::price_update::Price>,
  price: Box<pyth_solana_receiver_sdk::price_update::Price>,
}

impl PythPushOraclePriceFeed {
  /// Pyth push oracles are update using crosschain messages from pythnet There can be multiple
  /// pyth push oracles for a given feed_id. Marginfi allows using any pyth push oracle with a
  /// sufficient verification level and price age.
  ///
  /// Security assumptions:
  /// - The pyth-push-oracle account is owned by the pyth-solana-receiver program, checked in
  ///   `load_price_update_v2_checked`
  /// - The pyth-push-oracle account is a PriceUpdateV2 account, checked in
  ///   `load_price_update_v2_checked`
  /// - The pyth-push-oracle account has a minimum verification level, checked in
  ///   `get_price_no_older_than_with_custom_verification_level`
  /// - The pyth-push-oracle account has a valid feed_id, the pyth-solana-receiver program
  ///   enforces that the feed_id matches the pythnet feed_id, checked in
  ///     - pyth-push-oracle asserts that a valid price update has a matching feed_id with the
  ///       existing pyth-push-oracle update
  ///       https://github.com/pyth-network/pyth-crosschain/blob/94f1bd54612adc3e186eaf0bb0f1f705880f20a6/target_chains/solana/programs/pyth-push-oracle/src/lib.rs#L101
  ///     - pyth-solana-receiver set the feed_id directly from a pythnet verified price_update
  ///       message
  ///       https://github.com/pyth-network/pyth-crosschain/blob/94f1bd54612adc3e186eaf0bb0f1f705880f20a6/target_chains/solana/programs/pyth-solana-receiver/src/lib.rs#L437
  /// - The pyth-push-oracle account is not older than the max_age, checked in
  ///   `get_price_no_older_than_with_custom_verification_level`
  pub fn load_checked(account: &solana_account::Account, clock: &Clock, max_age: u64) -> MarginfiResult<Self> {
      let price_feed_account = load_price_update_v2_checked(account)?;
      let feed_id = &price_feed_account.price_message.feed_id;

      let price = price_feed_account
          .get_price_no_older_than_with_custom_verification_level(
              clock,
              max_age,
              feed_id,
              MIN_PYTH_PUSH_VERIFICATION_LEVEL,
          )
          .map_err(|e| {
              debug!("Pyth push oracle error: {:?}", e);
              let error: MarginfiError = e.into();
              error
          })?;

      let ema_price = {
          let price_update::PriceFeedMessage {
              exponent,
              publish_time,
              ema_price,
              ema_conf,
              ..
          } = price_feed_account.price_message;

          pyth_solana_receiver_sdk::price_update::Price {
              price: ema_price,
              conf: ema_conf,
              exponent,
              publish_time,
          }
      };

      Ok(Self {
          price: Box::new(price),
          ema_price: Box::new(ema_price),
      })
  }

  pub fn load_unchecked(account: &solana_account::Account) -> MarginfiResult<Self> {
      let price_feed_account = load_price_update_v2_checked(account)?;

      let price = price_feed_account
          .get_price_unchecked(&price_feed_account.price_message.feed_id)
          .map_err(|e| {
              println!("Pyth push oracle error: {:?}", e);
              let error: MarginfiError = e.into();
              error
          })?;

      let ema_price = {
          let price_update::PriceFeedMessage {
              exponent,
              publish_time,
              ema_price,
              ema_conf,
              ..
          } = price_feed_account.price_message;

          pyth_solana_receiver_sdk::price_update::Price {
              price: ema_price,
              conf: ema_conf,
              exponent,
              publish_time,
          }
      };

      Ok(Self {
          price: Box::new(price),
          ema_price: Box::new(ema_price),
      })
  }

  pub fn peek_feed_id(account: &solana_account::Account) -> MarginfiResult<FeedId> {
      let price_feed_account = load_price_update_v2_checked(account)?;

      Ok(price_feed_account.price_message.feed_id)
  }

  fn get_confidence_interval(
      &self,
      use_ema: bool,
      oracle_max_confidence: u32,
  ) -> MarginfiResult<I80F48> {
      let price = if use_ema {
          &self.ema_price
      } else {
          &self.price
      };

      let conf_interval =
          pyth_price_components_to_i80f48(I80F48::from_num(price.conf), price.exponent)?
              .checked_mul(CONF_INTERVAL_MULTIPLE)
              .ok_or_else(math_error!())?;

      let price = pyth_price_components_to_i80f48(I80F48::from_num(price.price), price.exponent)?;

      // Fail the price fetch if confidence > price * oracle_max_confidence
      let oracle_max_confidence = if oracle_max_confidence > 0 {
          I80F48::from_num(oracle_max_confidence)
      } else {
          // The default max confidence is 10%
          U32_MAX_DIV_10
      };
      let max_conf = price
          .checked_mul(oracle_max_confidence)
          .ok_or_else(math_error!())?
          .checked_div(U32_MAX)
          .ok_or_else(math_error!())?;
      if conf_interval > max_conf {
          let price = price.to_num::<f64>();
          let conf_interval = conf_interval.to_num::<f64>();
          let max_conf = max_conf.to_num::<f64>();
          msg!(
              "oracle price: {:?}, conf was {:?}, but max is {:?}",
              price,
              conf_interval,
              max_conf
          );
          return err!(MarginfiError::OracleMaxConfidenceExceeded);
      }

      // Cap confidence interval to 5% of price regardless
      let capped_conf_interval = price
          .checked_mul(MAX_CONF_INTERVAL)
          .ok_or_else(math_error!())?;

      assert!(
          capped_conf_interval >= I80F48::ZERO,
          "Negative max confidence interval"
      );

      assert!(
          conf_interval >= I80F48::ZERO,
          "Negative confidence interval"
      );

      Ok(min(conf_interval, capped_conf_interval))
  }

  #[inline(always)]
  fn get_ema_price(&self) -> MarginfiResult<I80F48> {
      pyth_price_components_to_i80f48(
          I80F48::from_num(self.ema_price.price),
          self.ema_price.exponent,
      )
  }

  #[inline(always)]
  fn get_unweighted_price(&self) -> MarginfiResult<I80F48> {
      pyth_price_components_to_i80f48(I80F48::from_num(self.price.price), self.price.exponent)
  }

  /// Find PDA address of a pyth push oracle given a shard_id and feed_id
  ///
  /// Pyth sponsored feed id
  /// `constants::PYTH_PUSH_PYTH_SPONSORED_SHARD_ID = 0`
  ///
  /// Marginfi sponsored feed id
  /// `constants::PYTH_PUSH_MARGINFI_SPONSORED_SHARD_ID = 3301`
  pub fn find_oracle_address(shard_id: u16, feed_id: &FeedId) -> (Pubkey, u8) {
      Pubkey::find_program_address(&[&shard_id.to_le_bytes(), feed_id], &PYTH_PUSH_ORACLE_ID)
  }
}

impl PriceAdapter for PythPushOraclePriceFeed {
  fn get_price_of_type(
      &self,
      price_type: OraclePriceType,
      bias: Option<PriceBias>,
      oracle_max_confidence: u32,
  ) -> MarginfiResult<I80F48> {
      let price = match price_type {
          OraclePriceType::TimeWeighted => self.get_ema_price()?,
          OraclePriceType::RealTime => self.get_unweighted_price()?,
      };

      match bias {
          None => Ok(price),
          Some(price_bias) => {
              let confidence_interval = self.get_confidence_interval(
                  matches!(price_type, OraclePriceType::TimeWeighted),
                  oracle_max_confidence,
              )?;

              match price_bias {
                  PriceBias::Low => Ok(price
                      .checked_sub(confidence_interval)
                      .ok_or_else(math_error!())?),
                  PriceBias::High => Ok(price
                      .checked_add(confidence_interval)
                      .ok_or_else(math_error!())?),
              }
          }
      }
  }
}

/// A slimmed down version of the PullFeedAccountData struct copied from the
/// switchboard-on-demand/src/pull_feed.rs
#[derive(Clone, Debug)]
pub struct LitePullFeedAccountData {
  pub result: CurrentResult,
  pub feed_hash: [u8; 32],
  pub last_update_timestamp: i64,
}

impl From<&PullFeedAccountData> for LitePullFeedAccountData {
  fn from(feed: &PullFeedAccountData) -> Self {
      Self {
          result: feed.result,
          feed_hash: feed.feed_hash,
          last_update_timestamp: feed.last_update_timestamp,
      }
  }
}

impl From<Ref<'_, PullFeedAccountData>> for LitePullFeedAccountData {
  fn from(feed: Ref<'_, PullFeedAccountData>) -> Self {
      Self {
          result: feed.result,
          feed_hash: feed.feed_hash,
          last_update_timestamp: feed.last_update_timestamp,
      }
  }
}

#[inline(always)]
fn pyth_price_components_to_i80f48(price: I80F48, exponent: i32) -> MarginfiResult<I80F48> {
  let scaling_factor = EXP_10_I80F48[exponent.unsigned_abs() as usize];

  let price = if exponent == 0 {
      price
  } else if exponent < 0 {
      price
          .checked_div(scaling_factor)
          .ok_or_else(math_error!())?
  } else {
      price
          .checked_mul(scaling_factor)
          .ok_or_else(math_error!())?
  };

  Ok(price)
}