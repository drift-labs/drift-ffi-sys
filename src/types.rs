//! cross-boundary FFI types
use std::collections::HashMap;

use abi_stable::std_types::RResult;
use drift_program::{
    controller::position::PositionDirection,
    math::{margin::MarginRequirementType, oracle::OracleValidity},
    state::{
        margin_calculation::MarginContext,
        oracle::OraclePriceData,
        order_params::PostOnlyParam,
        perp_market::PerpMarket,
        spot_market::SpotMarket,
        state::OracleGuardRails,
        user::{MarketType, OrderTriggerCondition, OrderType},
    },
};
use fxhash::FxBuildHasher;
use solana_sdk::{
    account::Account,
    account_info::{Account as _, AccountInfo, IntoAccountInfo},
    clock::Slot,
    pubkey::Pubkey,
};

#[repr(C)]
#[derive(Debug)]
pub struct AccountWithKey {
    pub key: Pubkey,
    pub account: Account,
}

impl From<(Pubkey, Account)> for AccountWithKey {
    fn from(value: (Pubkey, Account)) -> Self {
        Self {
            key: value.0,
            account: value.1,
        }
    }
}

impl From<AccountWithKey> for (Pubkey, Account) {
    fn from(value: AccountWithKey) -> Self {
        (value.key, value.account)
    }
}

impl<'a> IntoAccountInfo<'a> for &'a mut AccountWithKey {
    fn into_account_info(self) -> AccountInfo<'a> {
        let (lamports, data, owner, executable, rent_epoch) = self.account.get();
        AccountInfo::new(
            &self.key, false, false, lamports, data, owner, executable, rent_epoch,
        )
    }
}

/// FFI equivalent of an `AccountMap`
#[repr(C)]
pub struct AccountsList<'a> {
    pub perp_markets: &'a mut [AccountWithKey],
    pub spot_markets: &'a mut [AccountWithKey],
    pub oracles: &'a mut [AccountWithKey],
    pub oracle_guard_rails: Option<OracleGuardRails>,
    pub latest_slot: Slot,
}

/// FFI type-safe equivalent of `MarginContext`
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum MarginContextMode {
    StandardMaintenance,
    StandardInitial,
    StandardCustom(MarginRequirementType),
}

impl From<MarginContextMode> for MarginContext {
    fn from(value: MarginContextMode) -> Self {
        match value {
            MarginContextMode::StandardMaintenance => {
                MarginContext::standard(MarginRequirementType::Maintenance)
            }
            MarginContextMode::StandardInitial => {
                MarginContext::standard(MarginRequirementType::Initial)
            }
            MarginContextMode::StandardCustom(m) => MarginContext::standard(m),
        }
    }
}

#[repr(C)]
#[derive(Clone, Debug)]
pub struct MarginCalculation {
    pub total_collateral: compat::i128,
    pub margin_requirement: compat::u128,
    pub with_perp_isolated_liability: bool,
    pub with_spot_isolated_liability: bool,
    pub total_spot_asset_value: compat::i128,
    pub total_spot_liability_value: compat::u128,
    pub total_perp_liability_value: compat::u128,
    pub total_perp_pnl: compat::i128,
    pub isolated_margin_calculations: [IsolatedMarginCalculation; 8],
}

#[repr(C)]
#[derive(Debug, Clone)]
pub struct SimplifiedMarginCalculation {
    pub total_collateral: compat::i128,
    pub total_collateral_buffer: compat::i128,
    pub margin_requirement: compat::u128,
    pub margin_requirement_plus_buffer: compat::u128,
    pub isolated_margin_calculations: [IsolatedMarginCalculation; 8],
    pub with_perp_isolated_liability: bool,
    pub with_spot_isolated_liability: bool,
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
pub struct IsolatedMarginCalculation {
    pub margin_requirement: compat::u128,
    pub total_collateral: compat::i128,
    pub total_collateral_buffer: compat::i128,
    pub margin_requirement_plus_buffer: compat::u128,
    pub market_index: u16,
}

impl MarginCalculation {
    pub fn get_free_collateral(&self) -> u128 {
        (self.total_collateral.0 - self.margin_requirement.0 as i128) // cast ok, margin_requirement > 0
            .max(0)
            .try_into()
            .expect("fits u128")
    }
}

/// Same as drift program `OrderParams` but with `C` layout
#[repr(C)]
#[derive(Debug)]
pub struct OrderParams {
    pub order_type: OrderType,
    pub market_type: MarketType,
    pub direction: PositionDirection,
    pub user_order_id: u8,
    pub base_asset_amount: u64,
    pub price: u64,
    pub market_index: u16,
    pub reduce_only: bool,
    pub post_only: PostOnlyParam,
    pub bit_flags: u8,
    pub max_ts: Option<i64>,
    pub trigger_price: Option<u64>,
    pub trigger_condition: OrderTriggerCondition,
    pub oracle_price_offset: Option<i32>, // price offset from oracle for order (~ +/- 2147 max)
    pub auction_duration: Option<u8>,     // specified in slots
    pub auction_start_price: Option<i64>, // specified in price or oracle_price_offset
    pub auction_end_price: Option<i64>,   // specified in price or oracle_price_offset
}

impl From<&OrderParams> for drift_program::state::order_params::OrderParams {
    fn from(value: &OrderParams) -> Self {
        Self {
            order_type: value.order_type,
            market_type: value.market_type,
            direction: value.direction,
            user_order_id: value.user_order_id,
            base_asset_amount: value.base_asset_amount,
            price: value.price,
            market_index: value.market_index,
            reduce_only: value.reduce_only,
            post_only: value.post_only,
            bit_flags: value.bit_flags,
            max_ts: value.max_ts,
            trigger_price: value.trigger_price,
            trigger_condition: value.trigger_condition,
            oracle_price_offset: value.oracle_price_offset,
            auction_duration: value.auction_duration,
            auction_start_price: value.auction_start_price,
            auction_end_price: value.auction_end_price,
        }
    }
}

impl From<&drift_program::state::order_params::OrderParams> for OrderParams {
    fn from(value: &drift_program::state::order_params::OrderParams) -> Self {
        Self {
            order_type: value.order_type,
            market_type: value.market_type,
            direction: value.direction,
            user_order_id: value.user_order_id,
            base_asset_amount: value.base_asset_amount,
            price: value.price,
            market_index: value.market_index,
            reduce_only: value.reduce_only,
            post_only: value.post_only,
            bit_flags: value.bit_flags,
            max_ts: value.max_ts,
            trigger_price: value.trigger_price,
            trigger_condition: value.trigger_condition,
            oracle_price_offset: value.oracle_price_offset,
            auction_duration: value.auction_duration,
            auction_start_price: value.auction_start_price,
            auction_end_price: value.auction_end_price,
        }
    }
}

/// `MMOraclePriceData` with aligned `mm_exchange_diff_bps` for abi compatibility
pub struct MMOraclePriceData {
    pub mm_oracle_price: i64,
    pub mm_oracle_delay: i64,
    pub mm_oracle_validity: OracleValidity,
    pub mm_exchange_diff_bps: compat::u128,
    pub exchange_oracle_price_data: OraclePriceData,
    pub safe_oracle_price_data: OraclePriceData,
}

/// C-ABI compatible result type for drift FFI calls
pub type FfiResult<T> = RResult<T, u32>;

pub mod compat {
    //! ffi compatible input types

    /// rust 1.76.0 ffi compatible i128
    #[derive(Copy, Clone, Debug, PartialEq, Default)]
    #[repr(C, align(16))]
    pub struct i128(pub std::primitive::i128);

    impl From<std::primitive::i128> for self::i128 {
        fn from(value: std::primitive::i128) -> Self {
            Self(value)
        }
    }

    /// rust 1.76.0 ffi compatible u128
    #[derive(Copy, Clone, Debug, PartialEq, Default)]
    #[repr(C, align(16))]
    pub struct u128(pub std::primitive::u128);

    impl From<std::primitive::u128> for self::u128 {
        fn from(value: std::primitive::u128) -> Self {
            Self(value)
        }
    }
}

/// Simple HashMap-based implementation of market state
#[derive(Default)]
pub struct MarketState {
    pub spot_markets: HashMap<u16, SpotMarket, FxBuildHasher>,
    pub perp_markets: HashMap<u16, PerpMarket, FxBuildHasher>,
    pub spot_oracle_prices: HashMap<u16, OraclePriceData, FxBuildHasher>,
    pub perp_oracle_prices: HashMap<u16, OraclePriceData, FxBuildHasher>,
    pub spot_pyth_prices: HashMap<u16, i64, FxBuildHasher>, // Override spot with pyth price
    pub perp_pyth_prices: HashMap<u16, i64, FxBuildHasher>, // Override perp with pyth price
    pub pyth_oracle_diff_threshold_bps: u64, // Min bps diff to prefer pyth price over oracle. Defaults to 0 (always use pyth when set).
}

impl MarketState {
    pub fn get_spot_market(&self, market_index: u16) -> &SpotMarket {
        self.spot_markets.get(&market_index).unwrap()
    }

    pub fn get_perp_market(&self, market_index: u16) -> &PerpMarket {
        self.perp_markets.get(&market_index).unwrap()
    }

    pub fn get_spot_oracle_price(&self, market_index: u16) -> Option<&OraclePriceData> {
        self.spot_oracle_prices.get(&market_index)
    }

    pub fn get_perp_oracle_price(&self, market_index: u16) -> Option<&OraclePriceData> {
        self.perp_oracle_prices.get(&market_index)
    }

    pub fn get_spot_pyth_price(&self, market_index: u16) -> Option<OraclePriceData> {
        self.spot_pyth_prices
            .get(&market_index)
            .map(|&price| OraclePriceData {
                price,
                confidence: 0,
                delay: 0,
                has_sufficient_number_of_data_points: true,
                sequence_id: None,
            })
    }

    pub fn get_perp_pyth_price(&self, market_index: u16) -> Option<OraclePriceData> {
        self.perp_pyth_prices
            .get(&market_index)
            .map(|&price| OraclePriceData {
                price,
                confidence: 0,
                delay: 0,
                has_sufficient_number_of_data_points: true,
                sequence_id: None,
            })
    }

    pub fn set_spot_market(&mut self, market: SpotMarket) {
        self.spot_markets.insert(market.market_index, market);
    }

    pub fn set_perp_market(&mut self, market: PerpMarket) {
        self.perp_markets.insert(market.market_index, market);
    }

    pub fn set_spot_oracle_price(&mut self, market_index: u16, price_data: OraclePriceData) {
        self.spot_oracle_prices.insert(market_index, price_data);
    }

    pub fn set_perp_oracle_price(&mut self, market_index: u16, price_data: OraclePriceData) {
        self.perp_oracle_prices.insert(market_index, price_data);
    }

    pub fn set_spot_pyth_price(&mut self, market_index: u16, price_data: i64) {
        self.spot_pyth_prices.insert(market_index, price_data);
    }

    pub fn set_perp_pyth_price(&mut self, market_index: u16, price_data: i64) {
        self.perp_pyth_prices.insert(market_index, price_data);
    }
}
