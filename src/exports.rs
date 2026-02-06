//!
//! Define FFI for subset of drift program
//!
use std::time::{SystemTime, UNIX_EPOCH};

use abi_stable::std_types::{
    ROption,
    RResult::{RErr, ROk},
};
use anchor_lang::prelude::{AccountInfo, AccountLoader};
use drift_program::{
    controller::{position::PositionDirection, repeg::_update_amm},
    math::{self, amm::calculate_amm_available_liquidity, margin::MarginRequirementType},
    state::{
        oracle::{get_oracle_price as get_oracle_price_, OraclePriceData, OracleSource},
        oracle_map::OracleMap,
        order_params::PlaceOrderOptions,
        perp_market::{ContractType, PerpMarket, AMM},
        perp_market_map::PerpMarketMap,
        protected_maker_mode_config::ProtectedMakerParams,
        revenue_share::RevenueShareOrder,
        spot_market::{SpotBalanceType, SpotMarket},
        spot_market_map::SpotMarketMap,
        state::{FeeTier, State, ValidityGuardRails},
        user::{Order, PerpPosition, SpotPosition, User},
    },
};
use solana_sdk::{
    account::Account,
    account_info::IntoAccountInfo,
    clock::{Clock, Slot},
    pubkey::Pubkey,
};

use crate::{
    margin::IncrementalMarginCalculation,
    types::{
        compat::{self},
        AccountsList, FfiResult, IsolatedMarginCalculation, MMOraclePriceData, MarginCalculation,
        MarginContextMode, MarketState,
    },
};

/// Return the FFI crate version
#[no_mangle]
pub extern "C" fn ffi_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[no_mangle]
pub extern "C" fn oracle_get_oracle_price(
    oracle_source: OracleSource,
    price_oracle: &mut (Pubkey, Account),
    clock_slot: Slot,
) -> FfiResult<OraclePriceData> {
    to_ffi_result(
        get_oracle_price_(
            &oracle_source,
            &price_oracle.into_account_info(),
            clock_slot,
        )
        .map(|o| unsafe { std::mem::transmute(o) }),
    )
}

#[no_mangle]
pub extern "C" fn math_calculate_auction_price(
    order: &Order,
    slot: Slot,
    tick_size: u64,
    oracle_price: ROption<i64>,
    is_prediction_market: bool,
) -> FfiResult<u64> {
    to_ffi_result(math::auction::calculate_auction_price(
        order,
        slot,
        tick_size,
        oracle_price.into(),
        is_prediction_market,
    ))
}

#[no_mangle]
pub extern "C" fn math_calculate_margin_requirement_and_total_collateral_and_liability_info(
    user: &User,
    accounts: &mut AccountsList,
    margin_context: MarginContextMode,
) -> FfiResult<MarginCalculation> {
    let spot_accounts = accounts
        .spot_markets
        .iter_mut()
        .map(IntoAccountInfo::into_account_info)
        .collect::<Vec<_>>();
    let spot_map =
        SpotMarketMap::load(&Default::default(), &mut spot_accounts.iter().peekable()).unwrap();

    let perp_accounts = accounts
        .perp_markets
        .iter_mut()
        .map(IntoAccountInfo::into_account_info)
        .collect::<Vec<_>>();
    let perp_map =
        PerpMarketMap::load(&Default::default(), &mut perp_accounts.iter().peekable()).unwrap();

    let oracle_accounts = accounts
        .oracles
        .iter_mut()
        .map(IntoAccountInfo::into_account_info)
        .collect::<Vec<_>>();
    let mut oracle_map = OracleMap::load(
        &mut oracle_accounts.iter().peekable(),
        accounts.latest_slot,
        accounts.oracle_guard_rails,
    )
    .unwrap();

    let margin_calculation = drift_program::math::margin::calculate_margin_requirement_and_total_collateral_and_liability_info(
        user,
        &perp_map,
        &spot_map,
        &mut oracle_map,
        margin_context.into(),
    );

    // map to ffi compatible u/i128s
    let m = margin_calculation.map(|m| {
        let mut isolated_margin_calculations = [IsolatedMarginCalculation::default(); 8];
        for (idx, (k, v)) in m.isolated_margin_calculations.into_iter().enumerate() {
            isolated_margin_calculations[idx] = IsolatedMarginCalculation {
                market_index: k,
                margin_requirement: v.margin_requirement.into(),
                total_collateral: v.total_collateral.into(),
                total_collateral_buffer: v.total_collateral_buffer.into(),
                margin_requirement_plus_buffer: v.margin_requirement_plus_buffer.into(),
            };
        }
        MarginCalculation {
            total_collateral: m.total_collateral.into(),
            margin_requirement: m.margin_requirement.into(),
            with_perp_isolated_liability: m.with_perp_isolated_liability,
            with_spot_isolated_liability: m.with_spot_isolated_liability,
            total_spot_asset_value: m.total_spot_asset_value.into(),
            total_spot_liability_value: m.total_spot_liability_value.into(),
            total_perp_liability_value: m.total_perp_liability_value.into(),
            total_perp_pnl: m.total_perp_pnl.into(),
            isolated_margin_calculations,
        }
    });
    to_ffi_result(m)
}

#[no_mangle]
pub extern "C" fn math_calculate_net_user_pnl(
    amm: &AMM,
    oracle_price: i64,
) -> FfiResult<compat::i128> {
    to_ffi_result(
        drift_program::math::amm::calculate_net_user_pnl(amm, oracle_price).map(compat::i128),
    )
}

#[no_mangle]
pub extern "C" fn simulate_update_amm(
    market: &mut PerpMarket,
    state: &State,
    mm_oracle_price_data: MMOraclePriceData,
    now: u64,
    slot: Slot,
) -> FfiResult<compat::i128> {
    to_ffi_result(
        _update_amm(
            market,
            &unsafe { std::mem::transmute(mm_oracle_price_data) },
            state,
            now as i64,
            slot,
        )
        .map(|x| x.into()),
    )
}

#[no_mangle]
pub extern "C" fn math_calculate_base_asset_amount_for_amm_to_fulfill(
    order: &Order,
    market: &PerpMarket,
    limit_price: Option<u64>,
    override_fill_price: Option<u64>,
    existing_base_asset_amount: i64,
    fee_tier: &FeeTier,
) -> FfiResult<(u64, Option<u64>)> {
    let res = drift_program::math::orders::calculate_base_asset_amount_for_amm_to_fulfill(
        order,
        market,
        limit_price,
        override_fill_price,
        existing_base_asset_amount,
        fee_tier,
    );
    to_ffi_result(res)
}

#[no_mangle]
pub extern "C" fn orders_place_perp_order<'a>(
    user: &User,
    state: &State,
    order_params: &crate::types::OrderParams,
    accounts: &mut AccountsList,
    high_leverage_mode_config: Option<&'a AccountInfo<'a>>,
    revenue_share_order: &mut Option<&'a mut RevenueShareOrder>,
) -> FfiResult<bool> {
    let spot_accounts = accounts
        .spot_markets
        .iter_mut()
        .map(IntoAccountInfo::into_account_info)
        .collect::<Vec<_>>();
    let spot_map =
        SpotMarketMap::load(&Default::default(), &mut spot_accounts.iter().peekable()).unwrap();

    let perp_accounts = accounts
        .perp_markets
        .iter_mut()
        .map(IntoAccountInfo::into_account_info)
        .collect::<Vec<_>>();
    let perp_map =
        PerpMarketMap::load(&Default::default(), &mut perp_accounts.iter().peekable()).unwrap();

    let oracle_accounts = accounts
        .oracles
        .iter_mut()
        .map(IntoAccountInfo::into_account_info)
        .collect::<Vec<_>>();
    let mut oracle_map = OracleMap::load(
        &mut oracle_accounts.iter().peekable(),
        accounts.latest_slot,
        accounts.oracle_guard_rails,
    )
    .unwrap();

    // has no epoch info but this is un-required for order placement
    let local_clock = Clock {
        slot: accounts.latest_slot,
        epoch_start_timestamp: 0,
        epoch: 0,
        leader_schedule_epoch: 0,
        unix_timestamp: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64,
    };

    let hlm_loader = high_leverage_mode_config
        .map(|x| AccountLoader::try_from_unchecked(&drift_program::ID, x).unwrap());
    let res = drift_program::controller::orders::place_perp_order(
        state,
        &mut user.clone(),
        user.authority,
        &perp_map,
        &spot_map,
        &mut oracle_map,
        &hlm_loader,
        &local_clock,
        order_params.into(),
        PlaceOrderOptions::default(),
        revenue_share_order,
    );

    to_ffi_result(res.map(|_| true))
}

#[no_mangle]
pub extern "C" fn order_calculate_auction_params_for_trigger_order(
    order: &Order,
    oracle_price: &OraclePriceData,
    perp_market: Option<&PerpMarket>,
) -> FfiResult<(u8, i64, i64)> {
    to_ffi_result(
        drift_program::math::auction::calculate_auction_params_for_trigger_order(
            order,
            oracle_price,
            20,
            perp_market,
        ),
    )
}

#[no_mangle]
pub extern "C" fn order_is_limit_order(order: &Order) -> bool {
    order.is_limit_order()
}

#[no_mangle]
pub extern "C" fn order_get_limit_price(
    order: &Order,
    valid_oracle_price: Option<i64>,
    fallback_price: Option<u64>,
    slot: u64,
    tick_size: u64,
    is_prediction_market: bool,
    pmm_params: Option<ProtectedMakerParams>,
) -> FfiResult<Option<u64>> {
    to_ffi_result(order.get_limit_price(
        valid_oracle_price,
        fallback_price,
        slot,
        tick_size,
        is_prediction_market,
        pmm_params,
    ))
}

#[no_mangle]
pub extern "C" fn order_is_resting_limit_order(order: &Order, slot: u64) -> FfiResult<bool> {
    to_ffi_result(order.is_resting_limit_order(slot))
}

#[no_mangle]
pub extern "C" fn order_params_will_auction_params_sanitize(
    order_params: &crate::types::OrderParams,
    perp_market: &PerpMarket,
    oracle_price: i64,
    is_signed_msg: bool,
) -> FfiResult<bool> {
    let mut order_params: drift_program::state::order_params::OrderParams = order_params.into();
    to_ffi_result(order_params.update_perp_auction_params(perp_market, oracle_price, is_signed_msg))
}

#[no_mangle]
pub extern "C" fn order_params_update_perp_auction_params(
    order_params: &mut crate::types::OrderParams,
    perp_market: &PerpMarket,
    oracle_price: i64,
    is_signed_msg: bool,
) {
    // Convert to program type, update, then write back into the caller's struct
    let mut order_params_2: drift_program::state::order_params::OrderParams =
        (order_params as &crate::types::OrderParams).into();
    order_params_2.update_perp_auction_params(perp_market, oracle_price, is_signed_msg);
    *order_params = (&order_params_2).into();
}

#[no_mangle]
pub extern "C" fn perp_market_get_protected_maker_params(
    market: &PerpMarket,
) -> ProtectedMakerParams {
    market.get_protected_maker_params()
}

#[no_mangle]
pub extern "C" fn order_triggered(order: &Order) -> bool {
    order.triggered()
}

#[no_mangle]
pub extern "C" fn perp_market_get_margin_ratio(
    market: &PerpMarket,
    size: compat::u128,
    margin_type: MarginRequirementType,
    high_leverage_mode: bool,
) -> FfiResult<u32> {
    to_ffi_result(market.get_margin_ratio(size.0, margin_type, high_leverage_mode))
}

#[no_mangle]
pub extern "C" fn perp_market_get_open_interest(market: &PerpMarket) -> compat::u128 {
    market.get_open_interest().into()
}

#[no_mangle]
pub extern "C" fn perp_market_get_mm_oracle_price_data(
    market: &PerpMarket,
    oracle_price_data: OraclePriceData,
    clock_slot: u64,
    oracle_guard_rails: &ValidityGuardRails,
) -> FfiResult<MMOraclePriceData> {
    to_ffi_result(
        market
            .get_mm_oracle_price_data(oracle_price_data, clock_slot, oracle_guard_rails)
            .map(|m| MMOraclePriceData {
                mm_oracle_price: m._get_mm_oracle_price(),
                mm_oracle_delay: m.get_mm_oracle_delay(),
                mm_oracle_validity: m.get_mm_oracle_validity(),
                // the alignment of this u128 is different across the abi boundary
                mm_exchange_diff_bps: m.get_mm_exchange_diff_bps().into(),
                exchange_oracle_price_data: m.get_exchange_oracle_price_data(),
                safe_oracle_price_data: m.get_safe_oracle_price_data(),
            }),
    )
}

#[no_mangle]
pub extern "C" fn perp_market_get_trigger_price(
    market: &PerpMarket,
    oracle_price: i64,
    now: i64,
    use_median_price: bool,
) -> FfiResult<u64> {
    to_ffi_result(market.get_trigger_price(oracle_price, now, use_median_price))
}

#[no_mangle]
pub extern "C" fn perp_market_get_fallback_price(
    market: &PerpMarket,
    direction: PositionDirection,
    oracle_price: i64,
    seconds_til_order_expiry: i64,
) -> FfiResult<u64> {
    let res = match calculate_amm_available_liquidity(&market.amm, &direction) {
        Ok(liq) => {
            market
                .amm
                .get_fallback_price(&direction, liq, oracle_price, seconds_til_order_expiry)
        }
        Err(err) => Err(err),
    };
    to_ffi_result(res)
}

#[no_mangle]
pub extern "C" fn perp_position_get_unrealized_pnl(
    position: &PerpPosition,
    oracle_price: i64,
) -> FfiResult<compat::i128> {
    to_ffi_result(position.get_unrealized_pnl(oracle_price).map(compat::i128))
}

#[no_mangle]
pub extern "C" fn perp_position_get_claimable_pnl(
    position: &PerpPosition,
    oracle_price: i64,
    pnl_pool_excess: compat::i128,
) -> FfiResult<compat::i128> {
    to_ffi_result(
        position
            .get_claimable_pnl(oracle_price, pnl_pool_excess.0)
            .map(compat::i128),
    )
}

#[no_mangle]
pub extern "C" fn perp_position_is_available(position: &PerpPosition) -> bool {
    position.is_available()
}

#[no_mangle]
pub extern "C" fn perp_position_is_open_position(position: &PerpPosition) -> bool {
    position.is_open_position()
}

#[no_mangle]
pub extern "C" fn perp_position_worst_case_base_asset_amount(
    position: &PerpPosition,
    oracle_price: i64,
    contract_type: ContractType,
) -> FfiResult<compat::i128> {
    let res = position.worst_case_base_asset_amount(oracle_price, contract_type);
    to_ffi_result(res.map(compat::i128))
}

#[no_mangle]
pub extern "C" fn spot_market_get_asset_weight(
    market: &SpotMarket,
    size: compat::u128,
    oracle_price: i64,
    margin_requirement_type: MarginRequirementType,
) -> FfiResult<u32> {
    to_ffi_result(market.get_asset_weight(size.0, oracle_price, &margin_requirement_type))
}

#[no_mangle]
pub extern "C" fn spot_market_get_liability_weight(
    market: &SpotMarket,
    size: compat::u128,
    margin_requirement_type: MarginRequirementType,
) -> FfiResult<u32> {
    to_ffi_result(market.get_liability_weight(size.0, &margin_requirement_type))
}

#[no_mangle]
pub extern "C" fn spot_market_get_margin_ratio(
    market: &SpotMarket,
    margin_type: MarginRequirementType,
) -> FfiResult<u32> {
    to_ffi_result(market.get_margin_ratio(&margin_type))
}

#[no_mangle]
pub extern "C" fn spot_position_is_available(position: &SpotPosition) -> bool {
    position.is_available()
}

#[no_mangle]
pub extern "C" fn spot_position_get_signed_token_amount(
    position: &SpotPosition,
    market: &SpotMarket,
) -> FfiResult<compat::i128> {
    to_ffi_result(position.get_signed_token_amount(market).map(compat::i128))
}

#[no_mangle]
pub extern "C" fn spot_position_get_token_amount(
    position: &SpotPosition,
    market: &SpotMarket,
) -> FfiResult<compat::u128> {
    to_ffi_result(position.get_token_amount(market).map(compat::u128))
}

#[no_mangle]
pub extern "C" fn spot_balance_get_token_amount(
    balance: compat::u128,
    spot_market: &SpotMarket,
    balance_type: &SpotBalanceType,
) -> FfiResult<compat::u128> {
    to_ffi_result(
        drift_program::math::spot_balance::get_token_amount(balance.0, spot_market, balance_type)
            .map(compat::u128),
    )
}

#[no_mangle]
pub extern "C" fn user_get_spot_position(
    user: &User,
    market_index: u16,
) -> FfiResult<&SpotPosition> {
    to_ffi_result(user.get_spot_position(market_index))
}

#[no_mangle]
pub extern "C" fn user_get_perp_position(
    user: &User,
    market_index: u16,
) -> FfiResult<&PerpPosition> {
    to_ffi_result(user.get_perp_position(market_index))
}

#[no_mangle]
pub extern "C" fn user_update_perp_position_max_margin_ratio(
    user: &mut User,
    market_index: u16,
    margin_ratio: u16,
) -> FfiResult<()> {
    to_ffi_result(user.update_perp_position_max_margin_ratio(market_index, margin_ratio))
}

#[no_mangle]
pub extern "C" fn margin_calculate_simplified_margin_requirement(
    user: &User,
    market_state: &MarketState,
    margin_type: MarginRequirementType,
    margin_buffer: u32,
) -> FfiResult<crate::types::SimplifiedMarginCalculation> {
    let result = crate::margin::calculate_simplified_margin_requirement(
        user,
        market_state,
        margin_type,
        margin_buffer,
    );

    to_ffi_result(result.map(Into::into))
}

#[no_mangle]
pub extern "C" fn incremental_margin_calculation_from_user(
    user: &User,
    market_state: &MarketState,
    margin_type: MarginRequirementType,
    timestamp: u64,
    margin_buffer: u32,
) -> IncrementalMarginCalculation {
    IncrementalMarginCalculation::from_user(
        user,
        market_state,
        margin_type,
        timestamp,
        margin_buffer,
    )
}

#[no_mangle]
pub extern "C" fn incremental_margin_calculation_update_spot_position(
    this: &mut IncrementalMarginCalculation,
    spot_position: &SpotPosition,
    market_state: &MarketState,
    timestamp: u64,
) {
    this.update_spot_position(spot_position, market_state, timestamp);
}

#[no_mangle]
pub extern "C" fn incremental_margin_calculation_update_perp_position(
    this: &mut IncrementalMarginCalculation,
    perp_position: &PerpPosition,
    market_state: &MarketState,
    timestamp: u64,
) {
    this.update_perp_position(perp_position, market_state, timestamp);
}

//
// Helpers
//
/// Convert Drift program result into an FFI compatible version
#[inline]
pub(crate) fn to_ffi_result<T>(result: Result<T, drift_program::error::ErrorCode>) -> FfiResult<T> {
    match result {
        Ok(r) => ROk(r),
        Err(err) => RErr(err.into()),
    }
}
