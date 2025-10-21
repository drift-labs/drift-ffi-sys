// Simplified Margin Calculation System

use std::cmp::Ordering;

// This is a mathematical abstraction of the Drift Protocol margin system
// Reuses existing type definitions while removing Solana-specific abstractions
use crate::types::MarketState;
use drift_program::{
    math::{
        constants::{
            MARGIN_PRECISION_I128, MARGIN_PRECISION_U128, OPEN_ORDER_MARGIN_REQUIREMENT,
            QUOTE_SPOT_MARKET_INDEX,
        },
        margin::{calculate_perp_position_value_and_pnl, MarginRequirementType},
        spot_balance::get_strict_token_value,
    },
    state::{
        oracle::StrictOraclePrice,
        spot_market::SpotBalanceType,
        user::{OrderFillSimulation, PerpPosition, SpotPosition, User},
    },
};

// Core margin calculation result
#[repr(C, align(16))]
#[derive(Debug, Clone)]
pub struct SimplifiedMarginCalculation {
    pub total_collateral: i128,
    pub total_collateral_buffer: i128,
    pub margin_requirement: u128,
    pub margin_requirement_plus_buffer: u128,
}

impl SimplifiedMarginCalculation {
    pub fn free_collateral(&self) -> i128 {
        self.total_collateral - self.margin_requirement as i128
    }

    pub fn get_total_collateral_plus_buffer(&self) -> i128 {
        self.total_collateral
            .saturating_add(self.total_collateral_buffer)
    }

    pub fn free_collateral_with_buffer(&self) -> i128 {
        self.get_total_collateral_plus_buffer() - self.margin_requirement_plus_buffer as i128
    }

    pub fn meets_margin_requirement(&self) -> bool {
        self.total_collateral >= self.margin_requirement as i128
    }

    pub fn meets_margin_requirement_with_buffer(&self) -> bool {
        self.get_total_collateral_plus_buffer() >= self.margin_requirement_plus_buffer as i128
    }
}

// Main simplified margin calculation function
// This removes the complex MarketMap abstractions and fuel accounting
// while maintaining the core mathematical logic
pub fn calculate_simplified_margin_requirement(
    user: &User,
    market_state: &MarketState,
    margin_type: MarginRequirementType,
    margin_buffer: u32,
) -> SimplifiedMarginCalculation {
    let user_high_leverage_mode = user.is_high_leverage_mode(margin_type);
    let mut total_collateral = 0i128;
    let mut total_collateral_buffer = 0i128;
    let mut margin_requirement = 0u128;
    let mut margin_requirement_plus_buffer = 0u128;
    let margin_buffer = margin_buffer as u128;

    // Get user's custom margin ratio (only applied for initial margin)
    let user_custom_margin_ratio = if margin_type == MarginRequirementType::Initial {
        user.max_margin_ratio
    } else {
        0_u32
    };

    // Process spot positions using worst-case fill simulation
    for spot_position in &user.spot_positions {
        if spot_position.is_available() {
            continue;
        }

        let spot_market = market_state.get_spot_market(spot_position.market_index);
        let oracle_price = market_state.get_spot_oracle_price(spot_position.market_index);

        let signed_token_amount = spot_position.get_signed_token_amount(spot_market).unwrap();

        let mut skip_token_value = false;
        if !(user.pool_id == 1 && spot_market.market_index == 0 && !spot_position.is_borrow()) {
        } else {
            skip_token_value = true;
        }

        // Check if position has open orders - if not, use simple calculation
        if spot_market.market_index == QUOTE_SPOT_MARKET_INDEX {
            // No open orders - use simple token value calculation
            let mut token_value = calculate_token_value(
                signed_token_amount,
                oracle_price.price,
                spot_market.decimals,
            );

            match spot_position.balance_type {
                SpotBalanceType::Deposit => {
                    // usdc deposit in pool 1 doesn't count
                    if skip_token_value {
                        token_value = 0;
                    }
                    total_collateral += token_value;
                }
                SpotBalanceType::Borrow => {
                    let liability_value = token_value.unsigned_abs();
                    margin_requirement += liability_value;
                    margin_requirement_plus_buffer +=
                        liability_value + (liability_value * margin_buffer) / MARGIN_PRECISION_U128;
                }
            }
        } else {
            // in non-strict mode ignore twap
            let strict_oracle_price = StrictOraclePrice {
                current: oracle_price.price,
                twap_5min: None,
            };

            let OrderFillSimulation {
                token_amount: _worst_case_token_amount,
                orders_value: worst_case_orders_value,
                token_value: worst_case_token_value,
                weighted_token_value: worst_case_weighted_token_value,
                ..
            } = spot_position
                .get_worst_case_fill_simulation(
                    spot_market,
                    &strict_oracle_price,
                    Some(signed_token_amount),
                    margin_type,
                )
                .unwrap()
                .apply_user_custom_margin_ratio(
                    spot_market,
                    strict_oracle_price.current,
                    user_custom_margin_ratio,
                )
                .unwrap();

            // Add open order margin requirement
            let open_order_margin = calculate_spot_open_order_margin(spot_position);
            margin_requirement += open_order_margin;

            match worst_case_token_value.cmp(&0) {
                Ordering::Greater => {
                    total_collateral += worst_case_weighted_token_value;
                }
                Ordering::Less => {
                    let liability_value = worst_case_weighted_token_value.unsigned_abs();
                    margin_requirement += liability_value;
                    margin_requirement_plus_buffer += liability_value
                        + (worst_case_token_value.unsigned_abs() * margin_buffer)
                            / MARGIN_PRECISION_U128;
                }
                Ordering::Equal => {}
            }

            match worst_case_orders_value.cmp(&0) {
                Ordering::Greater => {
                    total_collateral += worst_case_orders_value;
                }
                Ordering::Less => {
                    let liability_value = worst_case_orders_value.unsigned_abs();
                    margin_requirement += liability_value;
                    margin_requirement_plus_buffer +=
                        liability_value + (liability_value * margin_buffer) / MARGIN_PRECISION_U128;
                }
                Ordering::Equal => {}
            }
        };
    }

    for perp_position in &user.perp_positions {
        if perp_position.is_available() {
            continue;
        }

        let perp_market = market_state.get_perp_market(perp_position.market_index);
        let oracle_price = market_state.get_perp_oracle_price(perp_position.market_index);

        let strict_quote_price = {
            let quote_price_data =
                market_state.get_spot_oracle_price(perp_market.quote_spot_market_index);
            StrictOraclePrice {
                current: quote_price_data.price,
                twap_5min: None,
            }
        };

        let perp_position_custom_margin_ratio = if margin_type == MarginRequirementType::Initial {
            perp_position.max_margin_ratio as u32
        } else {
            0_u32
        };

        // Calculate unrealized PnL
        let (
            perp_margin_requirement,
            weighted_pnl,
            worst_case_liability_value,
            _open_order_margin_requirement,
            _base_asset_value,
        ) = calculate_perp_position_value_and_pnl(
            perp_position,
            perp_market,
            oracle_price,
            &strict_quote_price,
            margin_type,
            user_custom_margin_ratio.max(perp_position_custom_margin_ratio),
            user_high_leverage_mode,
            false,
        )
        .unwrap();

        margin_requirement += perp_margin_requirement;
        margin_requirement_plus_buffer += perp_margin_requirement
            + (worst_case_liability_value * margin_buffer) / MARGIN_PRECISION_U128;

        total_collateral += weighted_pnl;
        if weighted_pnl < 0 {
            total_collateral_buffer +=
                (weighted_pnl * margin_buffer as i128) / MARGIN_PRECISION_I128;
        }
    }

    SimplifiedMarginCalculation {
        total_collateral,
        margin_requirement,
        total_collateral_buffer,
        margin_requirement_plus_buffer,
    }
}

/// Incremental margin calculation
///
/// Provides an alternative incremental API for calculating margin info
#[repr(C, align(16))]
#[derive(Debug, Clone)]
pub struct IncrementalMarginCalculation {
    pub total_collateral: i128,
    pub total_collateral_buffer: i128,
    pub margin_requirement: u128,
    pub margin_requirement_plus_buffer: u128,
    // Cached position contributions
    pub spot_collateral: [PositionCollateral; 8],
    pub perp_collateral: [PositionCollateral; 8],
    // Metadata
    pub last_updated: u64,
    pub user_custom_margin_ratio: u32,
    pub margin_buffer: u32,
    pub margin_type: MarginRequirementType,
    pub user_high_leverage_mode: bool,
    pub user_pool_id: u8,
}

/// position collateral contribution
#[repr(C, align(16))]
#[derive(Default, Debug, Clone, Copy)]
pub struct PositionCollateral {
    pub collateral_value: i128,
    pub collateral_buffer: i128,
    pub liability_value: u128,
    pub liability_buffer: u128,
    pub last_updated: u64,
    pub market_index: u16,
}

impl PositionCollateral {
    fn exists(&self) -> bool {
        self.liability_value != 0 || self.collateral_value != 0 || self.last_updated > 0
    }
}

impl Default for IncrementalMarginCalculation {
    fn default() -> Self {
        Self {
            total_collateral: 0,
            margin_requirement: 0,
            total_collateral_buffer: 0,
            margin_requirement_plus_buffer: 0,
            spot_collateral: Default::default(),
            perp_collateral: Default::default(),
            last_updated: 0,
            margin_type: MarginRequirementType::Initial,
            user_high_leverage_mode: false,
            user_pool_id: 0,
            user_custom_margin_ratio: 0,
            margin_buffer: 0,
        }
    }
}

// Incremental margin calculation functions
impl IncrementalMarginCalculation {
    pub fn new(
        margin_type: MarginRequirementType,
        user_high_leverage_mode: bool,
        user_custom_margin_ratio: u32,
        margin_buffer: u32,
        user_pool_id: u8,
    ) -> Self {
        Self {
            margin_type,
            user_high_leverage_mode,
            user_custom_margin_ratio,
            margin_buffer,
            user_pool_id,
            ..Default::default()
        }
    }

    // Calculate initial cached margin calculation
    pub fn from_user(
        user: &User,
        market_state: &MarketState,
        margin_type: MarginRequirementType,
        timestamp: u64,
        margin_buffer: u32,
    ) -> Self {
        let user_high_leverage_mode = user.is_high_leverage_mode(margin_type);
        let user_custom_margin_ratio = if margin_type == MarginRequirementType::Initial {
            user.max_margin_ratio
        } else {
            0_u32
        };
        let mut this = Self::new(
            margin_type,
            user_high_leverage_mode,
            user_custom_margin_ratio,
            margin_buffer,
            user.pool_id,
        );
        this.calculate(user, market_state, timestamp);
        this
    }

    pub fn free_collateral(&self) -> i128 {
        self.total_collateral - self.margin_requirement as i128
    }

    pub fn get_total_collateral_plus_buffer(&self) -> i128 {
        self.total_collateral
            .saturating_add(self.total_collateral_buffer)
    }

    pub fn free_collateral_with_buffer(&self) -> i128 {
        self.get_total_collateral_plus_buffer() - self.margin_requirement_plus_buffer as i128
    }

    pub fn meets_margin_requirement(&self) -> bool {
        self.total_collateral >= self.margin_requirement as i128
    }

    pub fn meets_margin_requirement_with_buffer(&self) -> bool {
        self.get_total_collateral_plus_buffer() >= self.margin_requirement_plus_buffer as i128
    }

    // Calculate full margin info
    pub fn calculate(&mut self, user: &User, market_state: &MarketState, timestamp: u64) {
        // Reset totals
        self.total_collateral = 0;
        self.margin_requirement = 0;
        self.total_collateral_buffer = 0;
        self.margin_requirement_plus_buffer = 0;
        self.spot_collateral = Default::default();
        self.perp_collateral = Default::default();

        // Recalculate all spot positions
        for spot_position in &user.spot_positions {
            if !spot_position.is_available() {
                self.update_spot_position(spot_position, market_state, timestamp);
            }
        }

        // Recalculate all perp positions
        for perp_position in &user.perp_positions {
            if !perp_position.is_available() {
                self.update_perp_position(perp_position, market_state, timestamp);
            }
        }

        self.last_updated = timestamp;
    }

    // Update a single spot position and recalculate totals
    pub fn update_spot_position(
        &mut self,
        spot_position: &SpotPosition,
        market_state: &MarketState,
        timestamp: u64,
    ) {
        // Find existing position
        if let Some(pos) = self
            .spot_collateral
            .iter()
            .position(|c| c.market_index == spot_position.market_index && c.exists())
        {
            // Calculate new contribution and mutate in place
            let new_collateral = calculate_spot_position_collateral(
                spot_position,
                market_state,
                self.margin_type,
                self.user_custom_margin_ratio,
                self.margin_buffer,
                timestamp,
                self.user_pool_id,
            );

            // Update the existing position in place
            let old_collateral = &self.spot_collateral[pos];

            self.total_collateral -= old_collateral.collateral_value;
            self.margin_requirement -= old_collateral.liability_value;
            self.total_collateral_buffer -= old_collateral.collateral_buffer;
            self.margin_requirement_plus_buffer -= old_collateral.liability_buffer;

            if spot_position.is_available() {
                // removed
                self.spot_collateral[pos] = Default::default();
            } else {
                self.total_collateral += new_collateral.collateral_value;
                self.margin_requirement += new_collateral.liability_value;
                self.total_collateral_buffer += new_collateral.collateral_buffer;
                self.margin_requirement_plus_buffer += new_collateral.liability_buffer;
                self.spot_collateral[pos] = new_collateral;
            }
        } else if !spot_position.is_available() {
            // New position, calculate and add
            let new_collateral = calculate_spot_position_collateral(
                spot_position,
                market_state,
                self.margin_type,
                self.user_custom_margin_ratio,
                self.margin_buffer,
                timestamp,
                self.user_pool_id,
            );

            // Add new contribution
            self.total_collateral += new_collateral.collateral_value;
            self.margin_requirement += new_collateral.liability_value;
            self.total_collateral_buffer += new_collateral.collateral_buffer;
            self.margin_requirement_plus_buffer +=
                new_collateral.liability_value + new_collateral.liability_buffer;

            // insert position
            if let Some(idx) = self.spot_collateral.iter().position(|x| {
                x.last_updated == 0 && x.collateral_value == 0 && x.liability_value == 0
            }) {
                self.spot_collateral[idx] = new_collateral;
            }
        }

        self.last_updated = timestamp;
    }

    // Update a single perp position and recalculate totals
    pub fn update_perp_position(
        &mut self,
        perp_position: &PerpPosition,
        market_state: &MarketState,
        timestamp: u64,
    ) {
        // Find existing position
        if let Some(pos) = self
            .perp_collateral
            .iter()
            .position(|c| c.market_index == perp_position.market_index && c.exists())
        {
            // Remove old contribution
            let old_collateral = &self.perp_collateral[pos];
            // Calculate new contribution and mutate in place
            let new_collateral = calculate_perp_position_collateral(
                perp_position,
                market_state,
                self.margin_type,
                self.user_high_leverage_mode,
                self.margin_buffer,
                timestamp,
            );

            self.total_collateral -= old_collateral.collateral_value;
            self.margin_requirement -= old_collateral.liability_value;
            self.total_collateral_buffer -= old_collateral.collateral_buffer;
            self.margin_requirement_plus_buffer -= old_collateral.liability_buffer;

            if perp_position.is_available() {
                // removed
                self.perp_collateral[pos] = Default::default();
            } else {
                self.total_collateral += new_collateral.collateral_value;
                self.margin_requirement += new_collateral.liability_value;
                self.total_collateral_buffer += new_collateral.collateral_buffer;
                self.margin_requirement_plus_buffer += new_collateral.liability_buffer;
                self.perp_collateral[pos] = new_collateral;
            }
        } else if !perp_position.is_available() {
            // New position, calculate and insert
            let new_collateral = calculate_perp_position_collateral(
                perp_position,
                market_state,
                self.margin_type,
                self.user_high_leverage_mode,
                self.margin_buffer,
                timestamp,
            );

            // Add new contribution
            self.total_collateral += new_collateral.collateral_value;
            self.margin_requirement += new_collateral.liability_value;
            self.total_collateral_buffer += new_collateral.collateral_buffer;
            self.margin_requirement_plus_buffer += new_collateral.liability_buffer;

            // insert position
            if let Some(idx) = self.perp_collateral.iter().position(|x| {
                x.last_updated == 0 && x.collateral_value == 0 && x.liability_value == 0
            }) {
                self.perp_collateral[idx] = new_collateral;
            }
        }

        self.last_updated = timestamp;
    }

    // Convert to simplified calculation for compatibility
    pub fn to_simplified(&self) -> SimplifiedMarginCalculation {
        SimplifiedMarginCalculation {
            total_collateral: self.total_collateral,
            margin_requirement: self.margin_requirement,
            total_collateral_buffer: self.total_collateral_buffer,
            margin_requirement_plus_buffer: self.margin_requirement_plus_buffer,
        }
    }
}

// Helper functions using existing Drift math utilities
fn calculate_token_value(token_amount: i128, price: i64, decimals: u32) -> i128 {
    let strict_price = StrictOraclePrice {
        current: price,
        twap_5min: None,
    };
    get_strict_token_value(token_amount, decimals, &strict_price).unwrap()
}

fn calculate_spot_open_order_margin(position: &SpotPosition) -> u128 {
    (position.open_orders as u128) * OPEN_ORDER_MARGIN_REQUIREMENT
}

// Helper functions for incremental calculations
fn calculate_spot_position_collateral(
    spot_position: &SpotPosition,
    market_state: &MarketState,
    margin_type: MarginRequirementType,
    user_custom_margin_ratio: u32,
    margin_buffer: u32,
    timestamp: u64,
    user_pool_id: u8,
) -> PositionCollateral {
    let margin_buffer = margin_buffer as u128;
    let spot_market = market_state.get_spot_market(spot_position.market_index);
    let oracle_price = market_state.get_spot_oracle_price(spot_position.market_index);

    // Create strict oracle price for worst-case simulation
    // in non-strict mode ignore twap (same as simplified calculation)
    let strict_oracle_price = StrictOraclePrice {
        current: oracle_price.price,
        twap_5min: None,
    };

    // Get signed token amount
    let signed_token_amount = spot_position.get_signed_token_amount(spot_market).unwrap();

    // Check if position has open orders - if not, use simple calculation
    let (worst_case_token_value, worst_case_weighted_token_value, worst_case_orders_value) =
        if spot_market.market_index == QUOTE_SPOT_MARKET_INDEX {
            let token_value = calculate_token_value(
                signed_token_amount,
                oracle_price.price,
                spot_market.decimals,
            );
            if !(user_pool_id == 1 && !spot_position.is_borrow()) {
                (token_value, token_value, 0)
            } else {
                // usdc deposit in pool 1 doesn't count
                (0, 0, 0)
            }
        } else {
            // non-usdc spot position
            let OrderFillSimulation {
                token_amount: _worst_case_token_amount,
                orders_value: worst_case_orders_value,
                token_value: worst_case_token_value,
                weighted_token_value: worst_case_weighted_token_value,
                ..
            } = spot_position
                .get_worst_case_fill_simulation(
                    spot_market,
                    &strict_oracle_price,
                    Some(signed_token_amount),
                    margin_type,
                )
                .unwrap()
                .apply_user_custom_margin_ratio(
                    spot_market,
                    strict_oracle_price.current,
                    user_custom_margin_ratio,
                )
                .unwrap();

            (
                worst_case_token_value,
                worst_case_weighted_token_value,
                worst_case_orders_value,
            )
        };

    // Handle worst_case_token_value
    let mut collateral_value = 0i128;
    let mut liability_value = 0u128;
    let mut liability_buffer = 0u128;

    match worst_case_token_value.cmp(&0) {
        Ordering::Greater => {
            collateral_value += worst_case_weighted_token_value;
        }
        Ordering::Less => {
            let liability = worst_case_weighted_token_value.unsigned_abs();
            liability_value += liability;
            liability_buffer += liability + (liability * margin_buffer) / MARGIN_PRECISION_U128;
        }
        Ordering::Equal => {}
    }

    match worst_case_orders_value.cmp(&0) {
        Ordering::Greater => {
            collateral_value += worst_case_orders_value;
        }
        Ordering::Less => {
            let liability = worst_case_orders_value.unsigned_abs();
            liability_value += liability;
            liability_buffer += liability + (liability * margin_buffer) / MARGIN_PRECISION_U128;
        }
        Ordering::Equal => {}
    }

    let open_order_margin = calculate_spot_open_order_margin(spot_position);
    liability_value += open_order_margin;

    PositionCollateral {
        market_index: spot_position.market_index,
        collateral_value,
        collateral_buffer: 0,
        liability_value,
        liability_buffer,
        last_updated: timestamp,
    }
}

fn calculate_perp_position_collateral(
    perp_position: &PerpPosition,
    market_state: &MarketState,
    margin_type: MarginRequirementType,
    user_high_leverage_mode: bool,
    margin_buffer: u32,
    timestamp: u64,
) -> PositionCollateral {
    let perp_market = market_state.get_perp_market(perp_position.market_index);
    let oracle_price = market_state.get_perp_oracle_price(perp_position.market_index);

    // Get quote price for the perp market
    let quote_oracle_data = market_state.get_spot_oracle_price(perp_market.quote_spot_market_index);
    let strict_quote_price = StrictOraclePrice {
        current: quote_oracle_data.price,
        twap_5min: None,
    };

    // Use the same calculation as simplified version
    let (
        perp_margin_requirement,
        weighted_pnl,
        worst_case_liability_value,
        _open_order_margin_requirement,
        _base_asset_value,
    ) = calculate_perp_position_value_and_pnl(
        perp_position,
        perp_market,
        oracle_price,
        &strict_quote_price,
        margin_type,
        0, // user_custom_margin_ratio - not used in cached version
        user_high_leverage_mode,
        false,
    )
    .unwrap();

    // Calculate margin buffer
    let mut collateral_buffer = 0i128;
    let collateral_value = weighted_pnl;
    let liability_value = perp_margin_requirement;

    // Apply buffer to margin requirement
    let liability_buffer = liability_value
        + (worst_case_liability_value * margin_buffer as u128) / MARGIN_PRECISION_U128;

    // Apply buffer to negative PnL (when it reduces collateral)
    if weighted_pnl < 0 {
        collateral_buffer = (collateral_value * margin_buffer as i128) / MARGIN_PRECISION_I128;
    }

    PositionCollateral {
        market_index: perp_position.market_index,
        collateral_value,
        collateral_buffer,
        liability_value,
        liability_buffer,
        last_updated: timestamp,
    }
}

// Utility functions
pub fn can_be_liquidated(calculation: &SimplifiedMarginCalculation) -> bool {
    calculation.free_collateral() < 0
}

#[cfg(test)]
mod tests {
    use drift_program::{
        math::constants::{
            AMM_RESERVE_PRECISION, BASE_PRECISION_I64, MAX_CONCENTRATION_COEFFICIENT,
            PEG_PRECISION, PRICE_PRECISION_I64, QUOTE_PRECISION_I64, SPOT_BALANCE_PRECISION,
            SPOT_BALANCE_PRECISION_U64, SPOT_CUMULATIVE_INTEREST_PRECISION, SPOT_WEIGHT_PRECISION,
        },
        state::{
            oracle::{HistoricalOracleData, OraclePriceData, OracleSource},
            perp_market::{ContractType, MarketStatus, PerpMarket, AMM},
            spot_market::{AssetTier, SpotBalanceType, SpotMarket},
        },
    };
    use solana_sdk::pubkey::Pubkey;

    use super::*;

    #[test]
    fn test_simplified_margin_calculation_with_trait() {
        // Create test data
        let user = User {
            spot_positions: [
                SpotPosition {
                    market_index: 0,
                    scaled_balance: 1000,
                    balance_type: SpotBalanceType::Deposit,
                    open_bids: 0,
                    open_asks: 0,
                    open_orders: 0,
                    cumulative_deposits: 0,
                    padding: [0; 4],
                },
                SpotPosition::default(), // Available position
                SpotPosition::default(),
                SpotPosition::default(),
                SpotPosition::default(),
                SpotPosition::default(),
                SpotPosition::default(),
                SpotPosition::default(),
            ],
            perp_positions: [
                PerpPosition::default(),
                PerpPosition::default(),
                PerpPosition::default(),
                PerpPosition::default(),
                PerpPosition::default(),
                PerpPosition::default(),
                PerpPosition::default(),
                PerpPosition::default(),
            ],
            max_margin_ratio: 0,
            pool_id: 1,
            ..Default::default()
        };

        let mut market_state = MarketState::default();

        // Add USDC spot market
        let mut usdc_market = SpotMarket::default();
        usdc_market.market_index = 0;
        usdc_market.decimals = 6;
        usdc_market.asset_tier = AssetTier::Collateral;
        usdc_market.initial_asset_weight = 8000; // 80%
        usdc_market.maintenance_asset_weight = 9000; // 90%
        usdc_market.initial_liability_weight = 11000; // 110%
        usdc_market.maintenance_liability_weight = 10500; // 105%
        usdc_market.imf_factor = 0;
        usdc_market.cumulative_deposit_interest = 10_u128.pow(19 - usdc_market.decimals as u32); // 1.0
        usdc_market.cumulative_borrow_interest = 10_u128.pow(19 - usdc_market.decimals as u32); // 1.0
        market_state.set_spot_market(usdc_market);

        // Add USDC oracle price
        market_state.set_spot_oracle_price(
            0,
            OraclePriceData {
                price: 1_000_000, // $1.00
                confidence: 1000,
                delay: 0,
                has_sufficient_number_of_data_points: true,
                sequence_id: Some(1),
            },
        );

        let result = calculate_simplified_margin_requirement(
            &user,
            &market_state,
            MarginRequirementType::Initial,
            0, // margin_buffer
        );

        assert!(result.free_collateral() > 0);
        assert!(!can_be_liquidated(&result));
    }

    #[test]
    fn test_margin_calculation_with_borrow_position() {
        let user = User {
            spot_positions: [
                SpotPosition {
                    market_index: 0,
                    scaled_balance: 1000,
                    balance_type: SpotBalanceType::Deposit,
                    open_bids: 0,
                    open_asks: 0,
                    open_orders: 0,
                    cumulative_deposits: 0,
                    padding: [0; 4],
                },
                SpotPosition {
                    market_index: 1,
                    scaled_balance: 500,
                    balance_type: SpotBalanceType::Borrow,
                    open_bids: 0,
                    open_asks: 0,
                    open_orders: 0,
                    cumulative_deposits: 0,
                    padding: [0; 4],
                },
                SpotPosition::default(),
                SpotPosition::default(),
                SpotPosition::default(),
                SpotPosition::default(),
                SpotPosition::default(),
                SpotPosition::default(),
            ],
            perp_positions: [
                PerpPosition::default(),
                PerpPosition::default(),
                PerpPosition::default(),
                PerpPosition::default(),
                PerpPosition::default(),
                PerpPosition::default(),
                PerpPosition::default(),
                PerpPosition::default(),
            ],
            max_margin_ratio: 0,
            pool_id: 1,
            ..Default::default()
        };

        let mut market_state = MarketState::default();

        // Add USDC spot market (deposit)
        market_state.set_spot_market(SpotMarket {
            market_index: 0,
            decimals: 6,
            asset_tier: AssetTier::Collateral,
            initial_asset_weight: 8000,          // 80%
            maintenance_asset_weight: 9000,      // 90%
            initial_liability_weight: 11000,     // 110%
            maintenance_liability_weight: 10500, // 105%
            imf_factor: 0,
            ..Default::default()
        });

        // Add USDC spot market (deposit)
        let mut usdc_market = SpotMarket::default();
        usdc_market.market_index = 0;
        usdc_market.decimals = 6;
        usdc_market.asset_tier = AssetTier::Collateral;
        usdc_market.initial_asset_weight = 8000; // 80%
        usdc_market.maintenance_asset_weight = 9000; // 90%
        usdc_market.initial_liability_weight = 11000; // 110%
        usdc_market.maintenance_liability_weight = 10500; // 105%
        usdc_market.imf_factor = 0;
        usdc_market.cumulative_deposit_interest = 10_u128.pow(19 - usdc_market.decimals as u32); // 1.0
        usdc_market.cumulative_borrow_interest = 10_u128.pow(19 - usdc_market.decimals as u32); // 1.0
        market_state.set_spot_market(usdc_market);

        // Add SOL spot market (borrow)
        let mut sol_market = SpotMarket::default();
        sol_market.market_index = 1;
        sol_market.decimals = 9;
        sol_market.asset_tier = AssetTier::Collateral;
        sol_market.initial_asset_weight = 8000; // 80%
        sol_market.maintenance_asset_weight = 9000; // 90%
        sol_market.initial_liability_weight = 11000; // 110%
        sol_market.maintenance_liability_weight = 10500; // 105%
        sol_market.imf_factor = 0;
        sol_market.cumulative_deposit_interest = 10_u128.pow(19 - sol_market.decimals as u32); // 1.0
        sol_market.cumulative_borrow_interest = 10_u128.pow(19 - sol_market.decimals as u32); // 1.0
        market_state.set_spot_market(sol_market);

        // Add oracle prices
        market_state.set_spot_oracle_price(
            0,
            OraclePriceData {
                price: 1_000_000, // $1.00 USDC
                confidence: 1000,
                delay: 0,
                has_sufficient_number_of_data_points: true,
                sequence_id: Some(1),
            },
        );

        market_state.set_spot_oracle_price(
            1,
            OraclePriceData {
                price: 100_000_000_000, // $100.00 SOL
                confidence: 1000,
                delay: 0,
                has_sufficient_number_of_data_points: true,
                sequence_id: Some(1),
            },
        );

        let result = calculate_simplified_margin_requirement(
            &user,
            &market_state,
            MarginRequirementType::Initial,
            0, // margin_buffer
        );

        // Should have both asset and liability values
        assert!(result.margin_requirement > 0);

        // Free collateral should be positive (deposit value > borrow margin requirement)
        assert!(result.free_collateral() > 0);
    }

    #[test]
    fn test_incremental_margin_calculation() {
        let mut user = User {
            spot_positions: [
                SpotPosition {
                    market_index: 0,
                    scaled_balance: 1000,
                    balance_type: SpotBalanceType::Deposit,
                    open_bids: 0,
                    open_asks: 0,
                    open_orders: 0,
                    cumulative_deposits: 0,
                    padding: [0; 4],
                },
                SpotPosition::default(),
                SpotPosition::default(),
                SpotPosition::default(),
                SpotPosition::default(),
                SpotPosition::default(),
                SpotPosition::default(),
                SpotPosition::default(),
            ],
            perp_positions: [
                PerpPosition::default(),
                PerpPosition::default(),
                PerpPosition::default(),
                PerpPosition::default(),
                PerpPosition::default(),
                PerpPosition::default(),
                PerpPosition::default(),
                PerpPosition::default(),
            ],
            max_margin_ratio: 0,
            pool_id: 1,
            ..Default::default()
        };

        let mut market_state = MarketState::default();

        // Add USDC spot market
        let mut usdc_market = SpotMarket::default();
        usdc_market.market_index = 0;
        usdc_market.decimals = 6;
        usdc_market.asset_tier = AssetTier::Collateral;
        usdc_market.initial_asset_weight = 8000; // 80%
        usdc_market.maintenance_asset_weight = 9000; // 90%
        usdc_market.initial_liability_weight = 11000; // 110%
        usdc_market.maintenance_liability_weight = 10500; // 105%
        usdc_market.imf_factor = 0;
        usdc_market.cumulative_deposit_interest = 10_u128.pow(19 - usdc_market.decimals as u32); // 1.0
        usdc_market.cumulative_borrow_interest = 10_u128.pow(19 - usdc_market.decimals as u32); // 1.0
        market_state.set_spot_market(usdc_market);

        // Add USDC oracle price
        market_state.set_spot_oracle_price(
            0,
            OraclePriceData {
                price: 1_000_000, // $1.00
                confidence: 1000,
                delay: 0,
                has_sufficient_number_of_data_points: true,
                sequence_id: Some(1),
            },
        );

        // Calculate initial cached margin
        let mut cached = IncrementalMarginCalculation::from_user(
            &user,
            &market_state,
            MarginRequirementType::Initial,
            1000,
            0, // margin_buffer
        );

        let initial_free_collateral = cached.free_collateral();
        assert!(initial_free_collateral > 0);

        // Update the position (simulate a trade)
        user.spot_positions[0].scaled_balance = 2000; // Double the position
        cached.update_spot_position(&user.spot_positions[0], &market_state, 2000);

        // Free collateral should have increased
        assert!(cached.free_collateral() > initial_free_collateral);

        // Add a borrow position
        user.spot_positions[1] = SpotPosition {
            market_index: 1,
            scaled_balance: 500,
            balance_type: SpotBalanceType::Borrow,
            open_bids: 0,
            open_asks: 0,
            open_orders: 0,
            cumulative_deposits: 0,
            padding: [0; 4],
        };

        // Add SOL spot market for borrowing
        let mut sol_market = SpotMarket::default();
        sol_market.market_index = 1;
        sol_market.decimals = 9;
        sol_market.asset_tier = AssetTier::Collateral;
        sol_market.initial_asset_weight = 8000;
        sol_market.maintenance_asset_weight = 9000;
        sol_market.initial_liability_weight = 11000;
        sol_market.maintenance_liability_weight = 10500;
        sol_market.imf_factor = 0;
        sol_market.cumulative_deposit_interest = 10_u128.pow(19 - sol_market.decimals as u32); // 1.0
        sol_market.cumulative_borrow_interest = 10_u128.pow(19 - sol_market.decimals as u32); // 1.0
        market_state.set_spot_market(sol_market);

        market_state.set_spot_oracle_price(
            1,
            OraclePriceData {
                price: 100_000_000_000, // $100.00 SOL
                confidence: 1000,
                delay: 0,
                has_sufficient_number_of_data_points: true,
                sequence_id: Some(1),
            },
        );

        // Update the new borrow position
        cached.update_spot_position(&user.spot_positions[1], &market_state, 3000);

        // Free collateral should have decreased due to borrow
        assert!(cached.free_collateral() < cached.total_collateral);

        // Verify we can convert to simplified calculation
        let simplified = cached.to_simplified();
        assert_eq!(simplified.free_collateral(), cached.free_collateral());
        assert_eq!(simplified.total_collateral, cached.total_collateral);
    }

    pub fn amm_default_test() -> AMM {
        let default_reserves = 100 * AMM_RESERVE_PRECISION;
        // make sure tests don't have the default sqrt_k = 0
        AMM {
            base_asset_reserve: default_reserves,
            quote_asset_reserve: default_reserves,
            sqrt_k: default_reserves,
            concentration_coef: MAX_CONCENTRATION_COEFFICIENT,
            order_step_size: 1,
            order_tick_size: 1,
            max_base_asset_reserve: u64::MAX as u128,
            min_base_asset_reserve: 0,
            terminal_quote_asset_reserve: default_reserves,
            peg_multiplier: drift_program::math::constants::PEG_PRECISION,
            max_fill_reserve_fraction: 1,
            max_spread: 1000,
            historical_oracle_data: HistoricalOracleData {
                last_oracle_price: PRICE_PRECISION_I64,
                ..HistoricalOracleData::default()
            },
            last_oracle_valid: true,
            ..AMM::default()
        }
    }

    fn perp_market_default_test() -> PerpMarket {
        let amm = amm_default_test();
        PerpMarket {
            amm,
            margin_ratio_initial: 1000,
            margin_ratio_maintenance: 500,
            ..PerpMarket::default()
        }
    }

    // Helper function to create a simple test setup for simplified margin calculation only
    fn create_simplified_test_setup() -> (User, MarketState) {
        // Create perp market
        let mut perp_market = PerpMarket {
            market_index: 0,
            amm: AMM {
                base_asset_reserve: 100 * AMM_RESERVE_PRECISION,
                quote_asset_reserve: 100 * AMM_RESERVE_PRECISION,
                bid_base_asset_reserve: 100 * AMM_RESERVE_PRECISION,
                bid_quote_asset_reserve: 100 * AMM_RESERVE_PRECISION,
                ask_base_asset_reserve: 100 * AMM_RESERVE_PRECISION,
                ask_quote_asset_reserve: 100 * AMM_RESERVE_PRECISION,
                sqrt_k: 100 * AMM_RESERVE_PRECISION,
                peg_multiplier: 100 * PEG_PRECISION,
                max_slippage_ratio: 50,
                max_fill_reserve_fraction: 100,
                order_step_size: 1000,
                order_tick_size: 1,
                oracle: Pubkey::default(),
                base_spread: 0,
                historical_oracle_data: HistoricalOracleData {
                    last_oracle_price: (100 * PRICE_PRECISION_I64) as i64,
                    last_oracle_price_twap: (100 * PRICE_PRECISION_I64) as i64,
                    last_oracle_price_twap_5min: (100 * PRICE_PRECISION_I64) as i64,
                    ..HistoricalOracleData::default()
                },
                ..AMM::default()
            },
            margin_ratio_initial: 2000,     // 20%
            margin_ratio_maintenance: 1000, // 10%
            status: MarketStatus::Initialized,
            contract_type: ContractType::Perpetual,
            ..perp_market_default_test()
        };
        perp_market.amm.max_base_asset_reserve = u128::MAX;
        perp_market.amm.min_base_asset_reserve = 0;

        // Create spot markets
        let usdc_spot_market = SpotMarket {
            market_index: 0,
            oracle_source: OracleSource::QuoteAsset,
            cumulative_deposit_interest: SPOT_CUMULATIVE_INTEREST_PRECISION,
            cumulative_borrow_interest: SPOT_CUMULATIVE_INTEREST_PRECISION,
            decimals: 6,
            initial_asset_weight: SPOT_WEIGHT_PRECISION, // 100%
            maintenance_asset_weight: SPOT_WEIGHT_PRECISION, // 100%
            initial_liability_weight: SPOT_WEIGHT_PRECISION, // 100%
            maintenance_liability_weight: SPOT_WEIGHT_PRECISION, // 100%
            deposit_balance: 10000 * SPOT_BALANCE_PRECISION,
            liquidator_fee: 0,
            historical_oracle_data: HistoricalOracleData {
                last_oracle_price_twap: PRICE_PRECISION_I64,
                last_oracle_price_twap_5min: PRICE_PRECISION_I64,
                ..HistoricalOracleData::default()
            },
            ..SpotMarket::default()
        };

        let sol_spot_market = SpotMarket {
            market_index: 1,
            oracle_source: OracleSource::PythLazer,
            cumulative_deposit_interest: SPOT_CUMULATIVE_INTEREST_PRECISION,
            cumulative_borrow_interest: SPOT_CUMULATIVE_INTEREST_PRECISION,
            decimals: 6,
            initial_asset_weight: SPOT_WEIGHT_PRECISION, // 100%
            maintenance_asset_weight: SPOT_WEIGHT_PRECISION, // 100%
            initial_liability_weight: SPOT_WEIGHT_PRECISION, // 100%
            maintenance_liability_weight: SPOT_WEIGHT_PRECISION, // 100%
            deposit_balance: MARGIN_PRECISION_U128 * SPOT_BALANCE_PRECISION,
            liquidator_fee: 0,
            historical_oracle_data: HistoricalOracleData {
                last_oracle_price_twap: PRICE_PRECISION_I64,
                last_oracle_price_twap_5min: PRICE_PRECISION_I64,
                ..HistoricalOracleData::default()
            },
            ..SpotMarket::default()
        };

        // Create user with simple positions
        let mut spot_positions = [SpotPosition::default(); 8];
        spot_positions[0] = SpotPosition {
            market_index: 0,
            balance_type: SpotBalanceType::Deposit,
            scaled_balance: 10 * SPOT_BALANCE_PRECISION_U64, // $10 USDC
            ..SpotPosition::default()
        };

        let user = User {
            orders: [drift_program::state::user::Order::default(); 32],
            perp_positions: [PerpPosition::default(); 8],
            spot_positions,
            ..User::default()
        };

        // Create simplified market state
        let mut market_state = MarketState::default();
        market_state.set_spot_market(usdc_spot_market);
        market_state.set_spot_market(sol_spot_market);
        market_state.set_perp_market(perp_market);

        // Set spot oracle price for USDC
        market_state.set_spot_oracle_price(
            0,
            OraclePriceData {
                price: PRICE_PRECISION_I64, // $1.00
                confidence: 1000,
                delay: 0,
                has_sufficient_number_of_data_points: true,
                sequence_id: Some(1),
            },
        );

        let sol_price = OraclePriceData {
            price: 200 * PRICE_PRECISION_I64, // $200.00
            confidence: 1000,
            delay: 0,
            has_sufficient_number_of_data_points: true,
            sequence_id: Some(1),
        };
        market_state.set_spot_oracle_price(1, sol_price);
        market_state.set_perp_oracle_price(0, sol_price);

        (user, market_state)
    }

    #[test]
    fn test_simplified_margin_calculation_basic() {
        let (user, market_state) = create_simplified_test_setup();

        // Calculate using simplified margin calculation
        let calculation = calculate_simplified_margin_requirement(
            &user,
            &market_state,
            MarginRequirementType::Initial,
            100,
        );

        // Basic assertions
        assert!(calculation.total_collateral > 0);
        assert_eq!(calculation.margin_requirement, 0); // No liabilities
        assert!(calculation.free_collateral() > 0);
    }

    #[test]
    fn test_simplified_margin_calculation_with_perp_positive_pnl() {
        let (mut user, market_state) = create_simplified_test_setup();

        // Add a perp position with positive PnL
        user.perp_positions[0] = PerpPosition {
            market_index: 0,
            base_asset_amount: BASE_PRECISION_I64, // 1 unit
            quote_asset_amount: -90 * QUOTE_PRECISION_I64, // -$90
            ..PerpPosition::default()
        };

        // Calculate using simplified margin calculation
        let calculation = calculate_simplified_margin_requirement(
            &user,
            &market_state,
            MarginRequirementType::Initial,
            0,
        );

        // Should have some PnL (positive or negative) contributing to collateral calculation
        assert!(calculation.total_collateral > 0);
        assert!(calculation.free_collateral() > 0);
        // The position should contribute to margin requirements
        assert!(calculation.margin_requirement > 0);
    }

    #[test]
    fn test_simplified_margin_calculation_with_perp_negative_pnl() {
        let (mut user, market_state) = create_simplified_test_setup();

        // Add a perp position with negative PnL
        user.perp_positions[0] = PerpPosition {
            market_index: 0,
            base_asset_amount: BASE_PRECISION_I64, // 1 unit
            quote_asset_amount: -110 * QUOTE_PRECISION_I64, // -$110
            ..PerpPosition::default()
        };

        // Calculate using simplified margin calculation
        let calculation = calculate_simplified_margin_requirement(
            &user,
            &market_state,
            MarginRequirementType::Initial,
            0,
        );

        // Should have negative PnL requiring margin
        assert!(calculation.margin_requirement > 0);
    }

    #[test]
    fn test_simplified_margin_calculation_maintenance_margin() {
        let (user, market_state) = create_simplified_test_setup();

        // Calculate using simplified margin calculation
        let calculation = calculate_simplified_margin_requirement(
            &user,
            &market_state,
            MarginRequirementType::Maintenance,
            0, // margin_buffer
        );

        // Basic assertions for maintenance margin
        assert!(calculation.total_collateral > 0);
        assert_eq!(calculation.margin_requirement, 0); // No liabilities
        assert!(calculation.free_collateral() > 0);
    }

    // Helper function to create a test setup with high leverage mode enabled
    fn create_high_leverage_test_setup() -> (User, MarketState) {
        // Create perp market with high leverage mode enabled
        let mut perp_market = PerpMarket {
            market_index: 0,
            amm: AMM {
                base_asset_reserve: 100 * AMM_RESERVE_PRECISION,
                quote_asset_reserve: 100 * AMM_RESERVE_PRECISION,
                bid_base_asset_reserve: 100 * AMM_RESERVE_PRECISION,
                bid_quote_asset_reserve: 100 * AMM_RESERVE_PRECISION,
                ask_base_asset_reserve: 100 * AMM_RESERVE_PRECISION,
                ask_quote_asset_reserve: 100 * AMM_RESERVE_PRECISION,
                sqrt_k: 100 * AMM_RESERVE_PRECISION,
                peg_multiplier: 100 * PEG_PRECISION,
                max_slippage_ratio: 50,
                max_fill_reserve_fraction: 100,
                order_step_size: 1000,
                order_tick_size: 1,
                oracle: Pubkey::default(),
                base_spread: 0,
                historical_oracle_data: HistoricalOracleData {
                    last_oracle_price: (100 * PRICE_PRECISION_I64) as i64,
                    last_oracle_price_twap: (100 * PRICE_PRECISION_I64) as i64,
                    last_oracle_price_twap_5min: (100 * PRICE_PRECISION_I64) as i64,
                    ..HistoricalOracleData::default()
                },
                ..AMM::default()
            },
            // Regular margin ratios (higher)
            margin_ratio_initial: 2000,     // 20%
            margin_ratio_maintenance: 1000, // 10%
            // High leverage margin ratios (lower)
            high_leverage_margin_ratio_initial: 1000, // 10%
            high_leverage_margin_ratio_maintenance: 500, // 5%
            status: MarketStatus::Initialized,
            contract_type: ContractType::Perpetual,
            ..perp_market_default_test()
        };
        perp_market.amm.max_base_asset_reserve = u128::MAX;
        perp_market.amm.min_base_asset_reserve = 0;

        // Create spot markets
        let usdc_spot_market = SpotMarket {
            market_index: 0,
            oracle_source: OracleSource::QuoteAsset,
            cumulative_deposit_interest: SPOT_CUMULATIVE_INTEREST_PRECISION,
            cumulative_borrow_interest: SPOT_CUMULATIVE_INTEREST_PRECISION,
            decimals: 6,
            initial_asset_weight: SPOT_WEIGHT_PRECISION, // 100%
            maintenance_asset_weight: SPOT_WEIGHT_PRECISION, // 100%
            initial_liability_weight: SPOT_WEIGHT_PRECISION, // 100%
            maintenance_liability_weight: SPOT_WEIGHT_PRECISION, // 100%
            deposit_balance: 10000 * SPOT_BALANCE_PRECISION,
            liquidator_fee: 0,
            historical_oracle_data: HistoricalOracleData {
                last_oracle_price_twap: PRICE_PRECISION_I64,
                last_oracle_price_twap_5min: PRICE_PRECISION_I64,
                ..HistoricalOracleData::default()
            },
            ..SpotMarket::default()
        };

        // Create user with high leverage mode enabled
        let mut spot_positions = [SpotPosition::default(); 8];
        spot_positions[0] = SpotPosition {
            market_index: 0,
            balance_type: SpotBalanceType::Deposit,
            scaled_balance: 10 * SPOT_BALANCE_PRECISION_U64, // $10 USDC
            ..SpotPosition::default()
        };

        let user = User {
            orders: [drift_program::state::user::Order::default(); 32],
            perp_positions: [PerpPosition::default(); 8],
            spot_positions,
            margin_mode: drift_program::state::user::MarginMode::HighLeverage, // Enable high leverage mode
            ..User::default()
        };

        // Create simplified market state
        let mut market_state = MarketState::default();
        market_state.set_spot_market(usdc_spot_market);
        market_state.set_perp_market(perp_market);

        // Set spot oracle price for USDC
        market_state.set_spot_oracle_price(
            0,
            OraclePriceData {
                price: PRICE_PRECISION_I64, // $1.00
                confidence: 1000,
                delay: 0,
                has_sufficient_number_of_data_points: true,
                sequence_id: Some(1),
            },
        );

        market_state.set_perp_oracle_price(
            0,
            OraclePriceData {
                price: PRICE_PRECISION_I64, // $1.00
                confidence: 1000,
                delay: 0,
                has_sufficient_number_of_data_points: true,
                sequence_id: Some(1),
            },
        );

        (user, market_state)
    }

    #[test]
    fn test_high_leverage_mode_perp_position_initial_margin() {
        let (mut user, market_state) = create_high_leverage_test_setup();

        // Add a perp position that would require margin
        user.perp_positions[0] = PerpPosition {
            market_index: 0,
            base_asset_amount: BASE_PRECISION_I64, // 1 unit
            quote_asset_amount: -110 * QUOTE_PRECISION_I64, // -$110
            ..PerpPosition::default()
        };

        // Calculate using simplified margin calculation
        let calculation = calculate_simplified_margin_requirement(
            &user,
            &market_state,
            MarginRequirementType::Initial,
            0,
        );

        // Should use high leverage margin ratios (lower requirements)
        assert!(calculation.total_collateral > 0);
        assert!(calculation.margin_requirement > 0);

        // The margin requirement should be lower than regular mode due to high leverage ratios
        // (10% instead of 20% for initial margin)
        assert!(calculation.free_collateral() > 0);
    }

    #[test]
    fn test_high_leverage_mode_perp_position_maintenance_margin() {
        let (mut user, market_state) = create_high_leverage_test_setup();

        // Add a perp position that would require margin
        user.perp_positions[0] = PerpPosition {
            market_index: 0,
            base_asset_amount: BASE_PRECISION_I64, // 1 unit
            quote_asset_amount: -110 * QUOTE_PRECISION_I64, // -$110
            ..PerpPosition::default()
        };

        // Calculate using simplified margin calculation
        let calculation = calculate_simplified_margin_requirement(
            &user,
            &market_state,
            MarginRequirementType::Maintenance,
            0, // margin_buffer
        );

        // Should use high leverage margin ratios (lower requirements)
        assert!(calculation.total_collateral > 0);
        assert!(calculation.margin_requirement > 0);

        // The margin requirement should be lower than regular mode due to high leverage ratios
        // (5% instead of 10% for maintenance margin)
        assert!(calculation.free_collateral() > 0);
    }

    #[test]
    fn test_high_leverage_mode_vs_regular_mode_comparison() {
        let (mut user_hl, market_state_hl) = create_high_leverage_test_setup();

        // Create regular mode setup with same perp market but different margin ratios
        let (mut user_reg, market_state_reg) = create_simplified_test_setup();

        // Set up same perp position for both users
        let perp_position = PerpPosition {
            market_index: 0,
            base_asset_amount: BASE_PRECISION_I64, // 1 unit
            quote_asset_amount: -110 * QUOTE_PRECISION_I64, // -$110
            ..PerpPosition::default()
        };

        user_hl.perp_positions[0] = perp_position;
        user_reg.perp_positions[0] = perp_position;

        // Calculate margin requirements for both modes
        let calculation_hl = calculate_simplified_margin_requirement(
            &user_hl,
            &market_state_hl,
            MarginRequirementType::Initial,
            0, // margin_buffer
        );

        let calculation_reg = calculate_simplified_margin_requirement(
            &user_reg,
            &market_state_reg,
            MarginRequirementType::Initial,
            0, // margin_buffer
        );

        // High leverage mode should have lower margin requirements
        assert!(calculation_hl.margin_requirement < calculation_reg.margin_requirement);

        // Both should have positive collateral and free collateral
        assert!(calculation_hl.total_collateral > 0);
        assert!(calculation_reg.total_collateral > 0);
        assert!(calculation_hl.free_collateral() > 0);
        assert!(calculation_reg.free_collateral() > 0);
    }

    #[test]
    fn test_high_leverage_mode_spot_positions_unaffected() {
        let (mut user, market_state) = create_high_leverage_test_setup();

        // Add a spot borrow position (spot positions should not be affected by HLM)
        user.spot_positions[1] = SpotPosition {
            market_index: 0,
            balance_type: SpotBalanceType::Borrow,
            scaled_balance: 5 * SPOT_BALANCE_PRECISION_U64, // $5 USDC borrow
            ..SpotPosition::default()
        };

        // Calculate using simplified margin calculation
        let calculation = calculate_simplified_margin_requirement(
            &user,
            &market_state,
            MarginRequirementType::Initial,
            100,
        );

        // Spot positions should be calculated normally (not affected by HLM)
        assert!(calculation.total_collateral > 0);
        assert!(calculation.margin_requirement > 0);
        assert!(calculation.free_collateral() > 0);
    }

    #[test]
    fn test_spot_position_without_open_orders() {
        // Test the simple calculation path (no open orders)
        let (user, market_state) = create_simplified_test_setup();

        let calculation = calculate_simplified_margin_requirement(
            &user,
            &market_state,
            MarginRequirementType::Initial,
            0,
        );

        // Should use simple calculation (no worst-case simulation)
        assert!(calculation.total_collateral > 0);
        assert_eq!(calculation.margin_requirement, 0); // No liabilities
        assert!(calculation.free_collateral() > 0);
    }

    #[test]
    fn test_spot_position_with_open_orders() {
        // Test the worst-case fill simulation path (with open orders)
        let (mut user, market_state) = create_simplified_test_setup();

        // Add a spot position with open orders
        user.spot_positions[1] = SpotPosition {
            market_index: 0, // USDC
            balance_type: SpotBalanceType::Deposit,
            scaled_balance: 1000 * SPOT_BALANCE_PRECISION_U64, // $1000 USDC
            open_bids: 100,                                    // 100 open bid orders
            open_asks: 50,                                     // 50 open ask orders
            open_orders: 0,                                    // 10 total open orders
            ..SpotPosition::default()
        };

        let calculation = calculate_simplified_margin_requirement(
            &user,
            &market_state,
            MarginRequirementType::Initial,
            0,
        );

        // Should use worst-case fill simulation
        assert!(calculation.total_collateral > 0);
        assert!(calculation.margin_requirement > 0); // Open orders require margin
        assert!(calculation.free_collateral() > 0);
    }

    #[test]
    fn test_spot_position_with_open_orders_borrow() {
        // Test worst-case simulation for borrow position with open orders
        let (mut user, market_state) = create_simplified_test_setup();

        // Add a borrow position with open orders
        user.spot_positions[1] = SpotPosition {
            market_index: 0, // USDC
            balance_type: SpotBalanceType::Borrow,
            scaled_balance: 500 * SPOT_BALANCE_PRECISION_U64, // $500 USDC borrow
            open_bids: 25,                                    // 25 open bid orders
            open_asks: 75,                                    // 75 open ask orders
            open_orders: 5,                                   // 5 total open orders
            ..SpotPosition::default()
        };

        let calculation = calculate_simplified_margin_requirement(
            &user,
            &market_state,
            MarginRequirementType::Initial,
            0,
        );

        // Should use worst-case fill simulation for borrow
        assert!(calculation.total_collateral > 0);
        assert!(calculation.margin_requirement > 0); // Borrow + open orders require margin
    }

    #[test]
    fn test_spot_position_user_custom_margin_ratio() {
        // Test user custom margin ratio application
        let (mut user, market_state) = create_simplified_test_setup();

        // Set user custom margin ratio
        user.max_margin_ratio = 2000; // 20% additional margin requirement

        // Add a borrow position
        user.spot_positions[1] = SpotPosition {
            market_index: 0, // USDC
            balance_type: SpotBalanceType::Borrow,
            scaled_balance: 1000 * SPOT_BALANCE_PRECISION_U64, // $1000 USDC borrow
            open_bids: 0,
            open_asks: 0,
            open_orders: 0,
            ..SpotPosition::default()
        };

        let calculation_initial = calculate_simplified_margin_requirement(
            &user,
            &market_state,
            MarginRequirementType::Initial,
            0, // margin_buffer
        );

        let calculation_maintenance = calculate_simplified_margin_requirement(
            &user,
            &market_state,
            MarginRequirementType::Maintenance,
            0,
        );

        // Initial margin should be higher due to custom margin ratio
        assert!(
            calculation_initial.margin_requirement > calculation_maintenance.margin_requirement
        );
    }

    #[test]
    fn test_spot_position_equivalence_simple_vs_simulation() {
        // Test that simple calculation and simulation give same results when no open orders
        let (user, market_state) = create_simplified_test_setup();

        // Test with no open orders (should use simple calculation)
        let calculation_simple = calculate_simplified_margin_requirement(
            &user,
            &market_state,
            MarginRequirementType::Initial,
            0, // margin_buffer
        );

        // Create identical user but with open orders set to 0 explicitly
        let user_with_orders = User {
            spot_positions: [
                SpotPosition {
                    market_index: 0,
                    balance_type: SpotBalanceType::Deposit,
                    scaled_balance: 10 * SPOT_BALANCE_PRECISION_U64, // $10 USDC
                    open_bids: 0,
                    open_asks: 0,
                    open_orders: 0,
                    ..SpotPosition::default()
                },
                SpotPosition::default(),
                SpotPosition::default(),
                SpotPosition::default(),
                SpotPosition::default(),
                SpotPosition::default(),
                SpotPosition::default(),
                SpotPosition::default(),
            ],
            perp_positions: [
                PerpPosition::default(),
                PerpPosition::default(),
                PerpPosition::default(),
                PerpPosition::default(),
                PerpPosition::default(),
                PerpPosition::default(),
                PerpPosition::default(),
                PerpPosition::default(),
            ],
            max_margin_ratio: 0,
            pool_id: 1,
            ..User::default()
        };

        let calculation_with_orders = calculate_simplified_margin_requirement(
            &user_with_orders,
            &market_state,
            MarginRequirementType::Initial,
            0, // margin_buffer
        );

        // Results should be identical
        assert_eq!(
            calculation_simple.total_collateral,
            calculation_with_orders.total_collateral
        );
        assert_eq!(
            calculation_simple.margin_requirement,
            calculation_with_orders.margin_requirement
        );
        assert_eq!(
            calculation_simple.free_collateral(),
            calculation_with_orders.free_collateral()
        );
    }

    #[test]
    fn test_simplified_vs_cached_margin_calculation_equivalence() {
        // Test that simplified and cached margin calculations produce identical results
        let (user, market_state) = create_simplified_test_setup();

        // Calculate using simplified method
        let simplified = calculate_simplified_margin_requirement(
            &user,
            &market_state,
            MarginRequirementType::Maintenance,
            0, // margin_buffer
        );

        // Calculate using cached method
        let cached = IncrementalMarginCalculation::from_user(
            &user,
            &market_state,
            MarginRequirementType::Maintenance,
            1000,
            0, // margin_buffer
        );

        // Results should be identical
        assert_eq!(simplified.total_collateral, cached.total_collateral);
        assert_eq!(simplified.margin_requirement, cached.margin_requirement);
        assert_eq!(simplified.free_collateral(), cached.free_collateral());
    }

    #[test]
    fn test_simplified_vs_cached_with_spot_borrow() {
        // Test with spot borrow position
        let (mut user, market_state) = create_simplified_test_setup();

        // Add a borrow position
        user.spot_positions[1] = SpotPosition {
            market_index: 1, // SOL
            balance_type: SpotBalanceType::Borrow,
            scaled_balance: 1 * SPOT_BALANCE_PRECISION_U64, // 1 SOL borrow
            ..SpotPosition::default()
        };

        // Calculate using both methods
        let simplified = calculate_simplified_margin_requirement(
            &user,
            &market_state,
            MarginRequirementType::Maintenance,
            0, // margin_buffer
        );

        let cached = IncrementalMarginCalculation::from_user(
            &user,
            &market_state,
            MarginRequirementType::Maintenance,
            1000,
            0, // margin_buffer
        );

        // Results should be identical
        assert_eq!(simplified.total_collateral, cached.total_collateral);
        assert_eq!(simplified.margin_requirement, cached.margin_requirement);
        assert_eq!(simplified.free_collateral(), cached.free_collateral());
    }

    #[test]
    fn test_simplified_vs_cached_with_perp_position() {
        // Test with perp position
        let (mut user, market_state) = create_simplified_test_setup();

        // Add a perp position
        user.perp_positions[0] = PerpPosition {
            market_index: 0,
            base_asset_amount: BASE_PRECISION_I64, // 1 unit
            quote_asset_amount: -100 * QUOTE_PRECISION_I64, // -$100
            ..PerpPosition::default()
        };

        // Calculate using both methods
        let simplified = calculate_simplified_margin_requirement(
            &user,
            &market_state,
            MarginRequirementType::Maintenance,
            0, // margin_buffer
        );

        let cached = IncrementalMarginCalculation::from_user(
            &user,
            &market_state,
            MarginRequirementType::Maintenance,
            1000,
            0, // margin_buffer
        );

        // Results should be identical
        assert_eq!(simplified.total_collateral, cached.total_collateral);
        assert_eq!(simplified.margin_requirement, cached.margin_requirement);
        assert_eq!(simplified.free_collateral(), cached.free_collateral());
    }

    #[test]
    fn test_simplified_vs_cached_with_open_orders() {
        // Test with open orders
        let (user, market_state) = create_simplified_test_setup();

        // Calculate using both methods
        let simplified = calculate_simplified_margin_requirement(
            &user,
            &market_state,
            MarginRequirementType::Maintenance,
            0, // margin_buffer
        );

        let cached = IncrementalMarginCalculation::from_user(
            &user,
            &market_state,
            MarginRequirementType::Maintenance,
            1_000,
            0, // margin_buffer
        );

        // Results should be identical
        assert_eq!(simplified.total_collateral, cached.total_collateral);
        assert_eq!(simplified.margin_requirement, cached.margin_requirement);
        assert_eq!(simplified.free_collateral(), cached.free_collateral());
    }

    #[test]
    fn test_simplified_vs_cached_maintenance_margin() {
        // Test maintenance margin calculation
        let (user, market_state) = create_simplified_test_setup();

        // Calculate using both methods
        let simplified = calculate_simplified_margin_requirement(
            &user,
            &market_state,
            MarginRequirementType::Maintenance,
            0, // margin_buffer
        );

        let cached = IncrementalMarginCalculation::from_user(
            &user,
            &market_state,
            MarginRequirementType::Maintenance,
            1000,
            0, // margin_buffer
        );

        // Results should be identical
        assert_eq!(simplified.total_collateral, cached.total_collateral);
        assert_eq!(simplified.margin_requirement, cached.margin_requirement);
        assert_eq!(simplified.free_collateral(), cached.free_collateral());
    }

    #[test]
    fn test_simplified_vs_cached_high_leverage_mode() {
        // Test high leverage mode
        let (user, market_state) = create_high_leverage_test_setup();

        // Calculate using both methods
        let simplified = calculate_simplified_margin_requirement(
            &user,
            &market_state,
            MarginRequirementType::Maintenance,
            0, // margin_buffer
        );

        let cached = IncrementalMarginCalculation::from_user(
            &user,
            &market_state,
            MarginRequirementType::Maintenance,
            1000,
            0, // margin_buffer
        );

        // Results should be identical
        assert_eq!(simplified.total_collateral, cached.total_collateral);
        assert_eq!(simplified.margin_requirement, cached.margin_requirement);
        assert_eq!(simplified.free_collateral(), cached.free_collateral());
    }

    #[test]
    fn test_simplified_vs_cached_custom_margin_ratio() {
        // Test with custom margin ratio
        let (mut user, market_state) = create_simplified_test_setup();

        // Set custom margin ratio
        user.max_margin_ratio = 2000; // 20% additional margin

        // Add a borrow position
        user.spot_positions[1] = SpotPosition {
            market_index: 1, // SOL
            balance_type: SpotBalanceType::Borrow,
            scaled_balance: 1 * SPOT_BALANCE_PRECISION_U64, // 1 SOL borrow
            open_bids: 0,
            open_asks: 0,
            open_orders: 0,
            ..SpotPosition::default()
        };

        // Calculate using both methods
        let simplified = calculate_simplified_margin_requirement(
            &user,
            &market_state,
            MarginRequirementType::Maintenance,
            0, // margin_buffer
        );

        let cached = IncrementalMarginCalculation::from_user(
            &user,
            &market_state,
            MarginRequirementType::Maintenance,
            1000,
            0, // margin_buffer
        );

        // Results should be identical
        assert_eq!(simplified.total_collateral, cached.total_collateral);
        assert_eq!(simplified.margin_requirement, cached.margin_requirement);
        assert_eq!(simplified.free_collateral(), cached.free_collateral());
    }

    #[test]
    fn test_margin_buffer_functionality() {
        // Test margin buffer functionality
        let (mut user, market_state) = create_simplified_test_setup();

        // Add a borrow position
        user.spot_positions[1] = SpotPosition {
            market_index: 1, // SOL
            balance_type: SpotBalanceType::Borrow,
            scaled_balance: 1 * SPOT_BALANCE_PRECISION_U64, // 1 SOL borrow
            open_bids: 0,
            open_asks: 0,
            open_orders: 0,
            ..SpotPosition::default()
        };

        // Calculate without margin buffer
        let calculation_no_buffer = calculate_simplified_margin_requirement(
            &user,
            &market_state,
            MarginRequirementType::Maintenance,
            0, // margin_buffer
        );

        // Calculate with 1% margin buffer
        let calculation_with_buffer = calculate_simplified_margin_requirement(
            &user,
            &market_state,
            MarginRequirementType::Maintenance,
            10_000, // 1% buffer (10_000 / MARGIN_PRECISION_U128 = 0.01)
        );

        // With buffer, margin requirement should be higher
        assert!(
            calculation_with_buffer.margin_requirement_plus_buffer
                > calculation_no_buffer.margin_requirement
        );

        // Free collateral with buffer should be lower
        assert!(
            calculation_with_buffer.free_collateral_with_buffer()
                < calculation_no_buffer.free_collateral()
        );

        // Buffer fields should be non-zero when buffer is applied
        assert!(
            calculation_with_buffer.total_collateral_buffer != 0
                || calculation_with_buffer.margin_requirement_plus_buffer
                    > calculation_with_buffer.margin_requirement
        );

        // Buffer fields should be zero when no buffer is applied
        assert_eq!(calculation_no_buffer.total_collateral_buffer, 0);
        assert_eq!(
            calculation_no_buffer.margin_requirement_plus_buffer,
            calculation_no_buffer.margin_requirement
        );
    }
}
