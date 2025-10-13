# CachedMarginCalculation FFI

This crate now exposes `CachedMarginCalculation` over FFI, allowing consumers to initialize it with a User account and call `update_spot_position` and `update_perp_position` on it.

## Available FFI Functions

### Constructor
- `cached_margin_calculation_from_user(user, market_state, margin_type, timestamp)` - Creates a new cached margin calculation from a user account

### Update Functions
- `cached_margin_calculation_update_spot_position(cached, spot_position, market_state, timestamp)` - Updates the cached calculation with a spot position change
- `cached_margin_calculation_update_perp_position(cached, perp_position, market_state, timestamp)` - Updates the cached calculation with a perp position change

### Getter Functions
- `cached_margin_calculation_get_total_collateral(cached)` - Returns total collateral
- `cached_margin_calculation_get_margin_requirement(cached)` - Returns margin requirement
- `cached_margin_calculation_get_free_collateral(cached)` - Returns free collateral
- `cached_margin_calculation_get_spot_asset_value(cached)` - Returns spot asset value
- `cached_margin_calculation_get_spot_liability_value(cached)` - Returns spot liability value
- `cached_margin_calculation_get_perp_pnl(cached)` - Returns perp PnL
- `cached_margin_calculation_get_perp_liability_value(cached)` - Returns perp liability value

## Usage Example

```rust
// Initialize cached margin calculation from user
let cached = cached_margin_calculation_from_user(
    &user,
    &market_state,
    MarginRequirementType::Initial,
    timestamp
)?;

// Update with spot position changes
cached_margin_calculation_update_spot_position(
    &mut cached,
    &spot_position,
    &market_state,
    timestamp
)?;

// Update with perp position changes
cached_margin_calculation_update_perp_position(
    &mut cached,
    &perp_position,
    &market_state,
    timestamp
)?;

// Access calculated values
let total_collateral = cached_margin_calculation_get_total_collateral(&cached);
let margin_requirement = cached_margin_calculation_get_margin_requirement(&cached);
let free_collateral = cached_margin_calculation_get_free_collateral(&cached);
```

## Benefits

- **Incremental Updates**: Only recalculate affected positions instead of full margin calculation
- **Performance**: Significantly faster for frequent position updates
- **Caching**: Maintains cached position contributions for efficient updates
- **FFI Safe**: All types are properly aligned for C interop