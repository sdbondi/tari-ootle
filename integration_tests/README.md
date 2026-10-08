# Integration Tests

This directory contains the integration tests for the Tari Ootle project using Cucumber for behavior-driven development (BDD).

## Running Tests

### Run All Tests

```bash
cargo test --release --test cucumber
```

### Run a Single Test by Name

To run a specific scenario by its name:

```bash
cargo test --release --test cucumber -- --name "Claim base layer burn funds with wallet daemon"
```

### Run Tests by Tag

```bash
cargo test --release --test cucumber -- --tags "@claim_burn"
```

### Scale Timeouts

Steps that wait for a condition poll it until a timeout. On a slow or loaded machine, stretch every timeout at once
with `CUCUMBER_TIMEOUT_SCALE` (a positive multiplier):

```bash
CUCUMBER_TIMEOUT_SCALE=2 cargo test --release --test cucumber
```

## Test Structure

- `tests/features/` - Contains `.feature` files with Cucumber scenarios
- `src/` - Contains step definitions and test implementation code
