use crate::{
  assert_struct_align, assert_struct_size,
};

use anchor_lang::prelude::Pubkey;

use bytemuck::{Pod, Zeroable};
use fixed::types::I80F48;

use super::{
  BankOperationalState, InterestRateConfig,
  OracleSetup, RiskTier
};
use super::super::WrappedI80F48;
use super::super::consts::{
  ASSET_TAG_DEFAULT, MAX_ORACLE_KEYS,
  TOTAL_ASSET_VALUE_INIT_LIMIT_INACTIVE,
  MAX_PYTH_ORACLE_AGE
};

assert_struct_size!(BankConfig, 544);
assert_struct_align!(BankConfig, 8);
#[repr(C)]
#[derive(Debug, PartialEq, Pod, Zeroable, Copy, Clone, Eq)]
pub struct BankConfig {
  /// TODO: Convert weights to (u64, u64) to avoid precision loss (maybe?)
  pub asset_weight_init: WrappedI80F48,
  pub asset_weight_maint: WrappedI80F48,

  pub liability_weight_init: WrappedI80F48,
  pub liability_weight_maint: WrappedI80F48,

  pub deposit_limit: u64,

  pub interest_rate_config: InterestRateConfig,
  pub operational_state: BankOperationalState,

  pub oracle_setup: OracleSetup,
  pub oracle_keys: [Pubkey; MAX_ORACLE_KEYS],

  // Note: Pubkey is aligned 1, so borrow_limit is the first aligned-8 value after deposit_limit
  pub _pad0: [u8; 6], // Bank state (1) + Oracle Setup (1) + 6 = 8

  pub borrow_limit: u64,

  pub risk_tier: RiskTier,

  /// Determines what kinds of assets users of this bank can interact with.
  /// Options:
  /// * ASSET_TAG_DEFAULT (0) - A regular asset that can be comingled with any other regular asset
  ///   or with `ASSET_TAG_SOL`
  /// * ASSET_TAG_SOL (1) - Accounts with a SOL position can comingle with **either**
  /// `ASSET_TAG_DEFAULT` or `ASSET_TAG_STAKED` positions, but not both
  /// * ASSET_TAG_STAKED (2) - Staked SOL assets. Accounts with a STAKED position can only deposit
  /// other STAKED assets or SOL (`ASSET_TAG_SOL`) and can only borrow SOL
  pub asset_tag: u8,

  /// Flags for various config options
  /// * 1 - Always set if bank created in 0.1.4 or later, or if migrated to the new pyth
  ///   oracle setup from a prior version. Not set in 0.1.3 or earlier banks using pyth that have
  ///   not yet migrated. Does nothing for banks that use switchboard.
  /// * 2, 4, 8, 16, etc - reserved for future use.
  pub config_flags: u8,

  pub _pad1: [u8; 5],

  /// USD denominated limit for calculating asset value for initialization margin requirements.
  /// Example, if total SOL deposits are equal to $1M and the limit it set to $500K,
  /// then SOL assets will be discounted by 50%.
  ///
  /// In other words the max value of liabilities that can be backed by the asset is $500K.
  /// This is useful for limiting the damage of orcale attacks.
  ///
  /// Value is UI USD value, for example value 100 -> $100
  pub total_asset_value_init_limit: u64,

  /// Time window in seconds for the oracle price feed to be considered live.
  pub oracle_max_age: u16,

  // pad to next 4-byte alignment to meet u32's requirements.
  pub _padding0: [u8; 2],

  /// From 0-100%, if the confidence exceeds this value, the oracle is considered invalid. Note:
  /// the confidence adjustment is capped at 5% regardless of this value.
  /// * 0 falls back to using the default 10% instead, i.e., U32_MAX_DIV_10
  /// * A %, as u32, e.g. 100% = u32::MAX, 50% = u32::MAX/2, etc.
  pub oracle_max_confidence: u32,

  /// Stored oracle price for `OracleSetup::Fixed`, otherwise does nothing
  pub fixed_price: WrappedI80F48,

  pub _padding1: [u8; 16],
}

impl BankConfig {
  #[inline]
  pub fn get_oracle_max_age(&self) -> u64 {
      match (self.oracle_max_age, self.oracle_setup) {
          (0, OracleSetup::PythPushOracle) => MAX_PYTH_ORACLE_AGE,
          (n, _) => n as u64,
      }
  }
}

impl Default for BankConfig {
  fn default() -> Self {
      Self {
          asset_weight_init: I80F48::ZERO.into(),
          asset_weight_maint: I80F48::ZERO.into(),
          liability_weight_init: I80F48::ONE.into(),
          liability_weight_maint: I80F48::ONE.into(),
          deposit_limit: 0,
          borrow_limit: 0,
          interest_rate_config: Default::default(),
          operational_state: BankOperationalState::Paused,
          oracle_setup: OracleSetup::None,
          oracle_keys: [Pubkey::default(); MAX_ORACLE_KEYS],
          _pad0: [0; 6],
          risk_tier: RiskTier::Isolated,
          asset_tag: ASSET_TAG_DEFAULT,
          config_flags: 0,
          _pad1: [0; 5],
          total_asset_value_init_limit: TOTAL_ASSET_VALUE_INIT_LIMIT_INACTIVE,
          oracle_max_age: 0,
          _padding0: [0; 2],
          oracle_max_confidence: 0,
          fixed_price: I80F48::ZERO.into(),
          _padding1: [0; 16],
      }
  }
}