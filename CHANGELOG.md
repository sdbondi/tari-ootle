# Changelog

All notable changes to this project will be documented in this file.
See [standard-version](https://github.com/conventional-changelog/standard-version) for commit guidelines.

## [0.40.0](https://github.com/tari-project/tari-ootle/compare/v0.39.3...v0.40.0) (2026-09-02)

Two production incidents on esmeralda are fixed here — a state-sync off-by-one that corrupted the
state tree and permanently wedged a validator, and the pool-clear that kept the same node out of
consensus for an epoch. Alongside them: the WASM metering ceiling is raised to make non-trivial
cryptography viable in templates, the substate schema activation schedule becomes per-network, and
the indexer's substate cache is invalidated by the transition stream instead of a two-second timer.

### ⚠️ Upgrade notes

- `feat!` — **All validators must be upgraded together.** `max_block_validation_execution_points`
  moves from 7.1e9 to 7.25e9 and `MAX_WASM_POINTS_PER_TRANSACTION` from 100M to 250M. These are
  consensus rules requiring network-wide uniformity — two validators on different versions disagree
  on block validity.
- `fix!` — **Reject reasons change, so transaction receipts change.** A transaction aborting because
  an input has no live version now reports `"Substate {id} is not found or DOWN"` instead of
  `"Substate {id} is DOWN"`. The receipt is a substate and is hashed into the state tree, so mixed
  versions commit different receipts for the same transaction and diverge.
- **Operator note** — a validator node now **refuses to start** if its binary introduces a substate
  schema activation at an epoch the node has already run past, since that would silently re-hash
  committed substates. Override with `--allow-past-protocol-activation` (or
  `validator_node.allow_past_protocol_activation`) only for a node whose state is being discarded.
  No existing network is affected: every schedule is still `[(Epoch(0), V0)]`.
- **Operator note** — **indexer config**: `latest_substate_cache_ttl` is removed, replaced by
  `substate_cache_max_serve_lag` (default 300s) and `substate_cache_max_entries` (default 100k). The
  internal `DEFAULT_CACHE_TTL` moves 300s → 900s and is demoted from a correctness mechanism to a
  coarse backstop for entries the transitions never reach.
  `max_serve_lag` must comfortably exceed a *full* sync round
  (`state_scanning_interval` plus the time to sync every shard group), not just the interval, or the
  cache closes between rounds. A SQLite migration runs on first start; the old `cacache` directory
  is orphaned and can be deleted.
- `feat!` — **`tari_indexer_client`**: `GetIndexerInfoResponse.latest_substate_cache_ttl_secs` is
  renamed `substate_cache_max_serve_lag_secs`.

### Consensus

- `fix` — **A validator whose pool was cleared can no longer be locked out of consensus for the rest
  of the epoch.** After a state sync, `Syncing::on_enter` clears the transaction pool; the next
  epoch's block re-proposed a cleared transaction, and `evaluate_local_only_command` no-votes any
  block whose transaction is not in the pool. The node fell behind and failed the state merkle root
  check on every subsequent block — ~65 minutes out of consensus on esmeralda. Parking does not
  cover it: the transaction *record* was never missing, only the derived pool record, and there is
  nothing to request from a peer. The readiness check now widens from "which transactions do we not
  have?" to "which are not ready?", re-sequencing a held transaction through the same
  `validate_new_transaction` path used for peer-fetched ones. That path already refuses to sequence
  an id whose finalized decision is a commit or whose `TransactionReceipt` exists in state, so a
  repair cannot resurrect what the synced state committed.
- `fix!` — **A destroyed substate and one that never existed report as the same error.** Telling them
  apart is a property of how much history a node retained, not of the ledger, but the distinction was
  stringified into the reject reason and hashed into the state tree — so two honest nodes with
  different `epoch_history_length` settings committed different receipts for the same transaction.
  `SubstateIsDown` is folded into `SubstateNotFound` on both `SubstateStoreError` and
  `LockFailedError`. Nothing depended on the distinction: `try_lock_all` already classified both as
  hard conflicts. This also removes a trap — `is_not_found_error()` never matched `SubstateIsDown`,
  so merging the message without merging the variant would have turned an ordinary stale submission
  into a fatal error propagating out of consensus.
- `feat` — **The substate schema activation schedule is per network.** Networks run at independent
  epochs, so one hardcoded table cannot express when a schema goes live on each.
  `ProtocolVersion::activations` matches exhaustively on `Network`, so a new network must state its
  own schedule rather than silently inherit one; `at`, `newest_scheduled_activation` and
  `hash_substate` take a `Network` alongside the `Epoch` already threaded to every hashing site. The
  duplicate `ProtocolVersion` in `common_types` is removed and re-exported from `engine_types`, so
  the copy gating consensus and the copy gating substate hashing can no longer disagree. The
  unsatisfiable `MAX_SUPPORTED` guard in the hotstuff worker is removed — it compared two values read
  from the same binary's table.

### State sync

- `fix` — **The state stream starts at the first *unpersisted* version.** It opened at the shard's
  already-persisted version, and the stream is inclusive, so the peer replayed a version the client
  had already written. JMT nodes are keyed by `(version, nibble_path)`: the rewrite overwrote the
  live nodes at those keys while recording those same keys as stale at that version, and an hour
  later the stale-node GC deleted them out from under the current tree. The node then failed every
  block evaluation and every subsequent sync on `A node v6384:f9 expected to exist … was not found`,
  looping `CheckSync → Syncing → Failure → Sleeping` permanently — unrecoverable without a database
  wipe. `calculate_substate_changes` now also rejects a non-monotonic version, so a same-version
  write fails loudly rather than silently corrupting the tree.
- `fix` — **`UP_ONLY` decides liveness by destruction, not by value presence.** A destroyed substate
  keeps its value until epoch GC prunes it, which runs `epoch_history_length` epochs back (default
  1), so anything destroyed inside that window was streamed as an up with its down filtered out and
  no later transition to correct it. An indexer syncing from scratch recorded those substates as live
  **permanently** — later rounds resume past that state version, so a UTXO spent shortly before the
  sync was reported unspent for the rest of that indexer's life.

### Execution Engine

- `feat!` — **The per-transaction WASM metering ceiling is raised from 100M to 250M points** (~12ms →
  ~30ms of validator CPU at the calibrated ~8.4M points/ms). The old ceiling put non-trivial
  cryptography out of reach of templates entirely and was 24x tighter than the native ceiling a
  single transaction already enjoys, for the same real CPU at the same price. Measured against a
  Groth16/BN254 verifier written entirely in WASM: at 100M nothing fitted except a single-input
  verification, at 96% of budget with nothing left for contract logic; at 250M a sixteen-input
  statement fits at 70% of budget and two verifies fit in one transaction. Sixty-four inputs
  deliberately does not fit — cost is linear at ~5.3M points each, so a statement that wide belongs
  behind a hash. The proposal budget is unchanged, so a block packs fewer heavy transactions rather
  than doing more work, and still admits at least 18 max-compute transactions
  (`MIN_MAX_COMPUTE_TRANSACTIONS_PER_BLOCK`). `FREE_COMPUTE_GRACE_POINTS` stays at 32M: nothing this
  expensive should be fundable before a fee is paid.
- `feat` — `TemplateTest::last_execution_points()` exposes what a call cost. `ExecuteResult` carries
  the points, but `call_function`/`call_method` return only the decoded value and drop the result.

### Indexer

- `feat` — **The substate cache is invalidated by the state transition stream instead of expiring on
  a timer**, so an unversioned read is servable indefinitely rather than for two seconds. Every read
  past that TTL cost a validator committee round trip — one per vault and per resource on a wallet
  account refresh. Presence in the cache is now validity, resting on three changes: the down feed is
  completed (`ALL_HASHES` was honoured on the up arm but not the down arm, so a subscriber never
  learned of a *terminal* down — a spent `Utxo` or `ConfidentialOutput`, or a `ValidatorFeePool`
  drained to zero — and would serve it as live forever); the cache moves from a `cacache` directory
  into the indexer's SQLite database so invalidation commits in the same transaction that advances
  `Key::SyncProgress`; and `ShardWatermarks` records per shard and **per process run** that a
  completion marker put the indexer level with the committee, so a fresh or restarted indexer serves
  nothing from cache until its first round lands. A cached head settles every version below it
  locally, answering a versioned read below the head without a round trip.
- `fix` — **The WASM module cache resolves against the configured data dir.** The indexer built its
  path from `config.indexer.data_dir` directly, which is relative by default and, unlike the
  validator node's, is never absolutised at load — so `wasm_cache` was created relative to the
  process working directory instead of `{base_path}/{network}/data/indexer/`.

### Swarm daemon

- `fix` — Log output cleaned up.

### Build & CI

- `ci` — **`consensus_tests` gets a 600s kill timeout**, up from the 240s `[profile.ci]` bound, scoped
  to that package alone. Runs were failing on `development` with 663 of 664 tests passing and one
  killed by the clock. These tests wait on wall-clock timers rather than on work finishing, so they
  are slow by construction and stretch further under runner contention — nineteen crossed the 60s
  slow mark within the same second, and several reported slow went on to pass at 73–93s.
- `test` — The walletd balance-change fixture builds its 205 bulk rows in one transaction instead of
  205, each of which was its own `with_write_tx` and so its own fsync. 4.3s → 0.6s locally, and the
  runtime no longer scales with fsync latency; it was timing out at 60s in CI while passing locally.
- `chore` — Dependency bumps: `taiki-e/install-action` 2.86.5 → 2.87.0, `browserslist` 4.28.6 →
  4.28.8 (swarm daemon web UI).

### Docs

- `docs` — Changelog entries added for 0.39.1, 0.39.2 and 0.39.3, which the changelog had skipped.

### Crate versions

`[workspace.package].version` moves to `0.40.0` (the whole tier-3 cohort). Independently versioned
crates affected:

| crate | version |
|---|---|
| `tari_ootle_wallet_crypto` | 0.41.0 → 0.42.0 |
| `tari_indexer_client` | 0.40.0 → 0.41.0 |
| `ootle-rs` | 0.21.0 → 0.22.0 |
| `tari_ootle_wallet_sdk` | 0.41.0 → 0.42.0 |
| `tari_ootle_wallet_storage_sqlite` | 0.41.0 → 0.42.0 |
| `tari_ootle_walletd_client` | 0.41.0 → 0.42.0 |

## [0.39.3](https://github.com/tari-project/tari-ootle/compare/v0.39.2...v0.39.3) (2026-08-26)

### ⚠️ Upgrade notes

- `feat!` — **Validators and indexers must be upgraded together.** The `sync_state` RPC now takes a list of
  `(shard, start_state_version)` cursors instead of a single shard, and its batch/completion messages
  carry the shard they belong to. The protocol name is unversioned, so a mixed-version pair cannot sync.
- `feat!` — **`tari_indexer_client`**: `TransactionEntry` gains a non-optional `source` field, so
  struct-literal construction breaks for downstream consumers. `ListRecentTransactionsRequest` gains an
  optional `source` filter.
- **Operator note** — indexer `transaction_retention_epochs` now defaults to `Some(50)` instead of retaining
  forever. Transaction rows written before the retention column existed carry epoch 0, so the first pruner
  pass after upgrading clears that backlog — whether or not gossip indexing is enabled.

### Indexer

- `feat` — **Transactions are indexed from network gossip**, not just from direct submissions. The indexer
  joins the transaction gossip topic as a full mesh participant: it validates what it receives, reports a
  verdict that propagates the transaction onward, and stores it. New config `index_gossiped_transactions`
  (default true) and `max_transaction_gossip_queue_bytes` (128 MB); `index_gossiped_transactions` is
  reported on `/info`, and metrics for received/accepted/rejected/ignored/stored/dropped plus queue depth
  are exposed behind the `metrics` feature.
  The stored transaction set is explicitly **best effort** — an indexer misses whatever was gossiped while
  it was offline or its queue was full, and nothing backfills it. Receipts for committed transactions stay
  complete from genesis; transaction bodies and never-committed transactions do not.
- `feat!` — `source` on `TransactionEntry` records where a transaction was learned of, with an optional
  `source` filter on the recent-transactions listing. A direct submission upgrades a row already stored
  from gossip.
- `feat` — **At most two state-sync streams per shard group** instead of one per shard — previously 257
  serialized round-trips per round at `P256`, roughly 21s at 80ms RTT against a 30s work interval.
  Per-shard progress is still recorded as completion markers arrive, so an interrupted stream resumes from
  where it got to. A truncated stream is now an error rather than silent success.
- `fix` — **Default substate rate limit raised** — wallets hit the limiter during a normal poll.

### Consensus

- `feat!` — **`sync_state` streams many shards over one stream.** Cursors must be non-empty, at most
  `num_preshards + 1`, in range, strictly ascending, and start above version 0; the responder streams each
  shard contiguously and closes it with its own marker. Verification granularity is unchanged — checkpoints
  already carry per-shard tree roots. The validator passes a single-element cursor list, so its sync
  behaviour is unchanged; ranged validator sync follows separately.

### Wallet

- `fix` — **Bindings**: `shortenString` returns short strings unchanged instead of producing overlapping
  output such as `Rick Ast...k Astley` in NFT metadata cards. Long addresses, hashes and Substate IDs keep
  their existing format.

### Swarm daemon

- `fix` — **Mining stops at the validator activation epoch.** Mining a fixed 20 blocks after registration
  crossed two epoch boundaries with 7 validators, so no committee ever ran consensus in the activation
  epoch and no checkpoint was ever written for it — every cold-starting validator then looped
  `Syncing → Failure → Sleeping` forever. The daemon now mines to a computed height and polls the validators
  for activation instead of mining further.
- `feat` — **Web UI redesigned around divergence.** A consensus spine shows validators as channels on a
  shared rail at the committee tip, so a lagging validator falls off it by a distance sized to its block
  deficit; a pool matrix replaces the per-node "transactions from other pools" tables. App shell with
  sidebar navigation, a live status bar and pages for validators, wallets, indexers, base layer and
  instances. Every feature of the old UI is kept.
- `fix` — One shared polling loop replaces per-card 1s timers; RPC failures surface as toasts; the log viewer
  gains level filters, search, follow and wrap; instance data deletion calls the method that actually
  exists (`delete_data`); reading `final_decision` no longer throws on unfinalized transactions;
  `npm run dev` proxies to the daemon.

### Build & CI

- `build` — **`tari_comms`, `tari_core` and `tari_p2p` are gone from the walletd, indexer and validator node
  dependency graphs.** `minotari_app_grpc` is now depended on with `default-features = false`; only its
  generated protobuf types were ever used. `tari_watcher`, `tari_swarm_daemon` and `integration_tests`
  still pull the wrapper grpc client crates, so a full `--workspace` build still unifies the feature on.
- `ci` — The `windows-arm64` binary build enters the `amd64_arm64` MSVC developer environment, so
  `liblmdb-sys`' bare `cl.exe` fallback resolves.

### Docs

- `docs` — New template-testing tutorial covering account setup, epoch and epoch-hash overrides, direct
  component-state inspection and rejected-transaction error assertions, backed by executable engine tests
  and a snippet-drift check.

### Crate versions

`[workspace.package].version` moves to `0.39.3` (the whole tier-3 cohort). Independently versioned crates
affected by the breaking `tari_indexer_client` change:

| crate | version |
|---|---|
| `tari_indexer_client` | 0.39.0 → 0.40.0 |
| `ootle-rs` | 0.20.0 → 0.21.0 |
| `tari_ootle_wallet_sdk` | 0.40.1 → 0.41.0 |
| `tari_ootle_wallet_storage_sqlite` | 0.40.0 → 0.41.0 |
| `tari_ootle_walletd_client` | 0.40.0 → 0.41.0 |

## [0.39.2](https://github.com/tari-project/tari-ootle/compare/v0.39.1...v0.39.2) (2026-08-24)

A hardening release: several Byzantine-reachable liveness bugs in consensus, a privilege escalation in
the wallet daemon, two engine metering/scoping gaps, and storage read paths that returned data from
branches or tables they should never have seen.

### ⚠️ Upgrade notes

- **A RocksDB migration (`v1`) runs on first start.** The substate-lock substate-id index now encodes its
  table prefix, so existing entries are rewritten. The migration is idempotent and safe to interrupt;
  a fresh database skips it. Timing is logged.

### Consensus

- `fix` — **Proposal votes are aggregated per voted block.** Votes were bucketed by `(epoch, height)` only,
  but a `ProposalVote`'s signature binds `(block_id, decision)` and its `block_height` is
  attacker-controlled. One Byzantine member voting for an invented `block_id` at the current height got
  its signature folded into the honest block's certificate, which every peer then rejected — repeatable
  every height, halting the chain well below the fault threshold. Safety was never at risk.
- `fix` — **The zero-block QC exemption requires genuine genesis shape.** It keyed off an all-zero header
  hash alone, so a peer could skip signature and quorum validation entirely with an arbitrary
  height/parent and push a receiving validator into `FallenBehind` catch-up. It now also requires a
  `ProposalCertificate` at height 0 with a zero parent; timeout certificates are never exempt.
- `fix` — **`MissingTransactionsRequest` is capped at 1000 transaction ids**, mirroring the bound the
  response path already applied. One small request could otherwise run an unbounded number of blocking
  store lookups inline on the consensus worker thread and echo back a correspondingly huge reply.

### State store

- `fix` — **Range query scans are bounded to their logical table.** Logical tables share a physical column
  family, separated by a leading prefix byte, and two of the four range methods left one end open.
  **Epoch GC therefore never made progress**: a single stored foreign proposal failed the scan, rolling
  back the whole cleanup transaction — block, QC and finalized-transaction pruning included — so the
  database grew without bound.
- `fix` — **Block diff queries are scoped to the queried branch.** The branch filter tested the query's own
  argument rather than the block that recorded the entry, so pending substate state from forked-out
  branches leaked into the evaluation of blocks on the surviving branch — the root cause of the flaky
  `catch_up_rewind_below_leaf_recovers` failure. Same-version changes now order `(version, is_down)`, so a
  DOWN supersedes its UP rather than the winner depending on block-id iteration order.
- `fix` — **The committed substate lock lookup returns the most recent lock**, not an arbitrary index match
  that could be superseded and then feed `try_lock`'s conflict decisions.
- `fix` — **The substate-lock substate-id index carries its table prefix** (migration `v1`, above). Latent
  until a new `SubstateId` variant reached the prefix range, at which point it would have silently
  overlapped another table.
- `fix` — `parked_block_remove_missing_transaction` uses the query-aware key iterator, so a decode error is
  propagated instead of being read as "still missing".

### Wallet

- `fix` — **`webrtc.start` no longer mints session tokens above the caller's own grant.** It parsed a
  caller-chosen permission set out of the request body and signed it verbatim — including
  `Permission::Admin`, which satisfies every check in the daemon. An integration holding only the
  deliberately least-powerful `webrtc` scope could escalate to a full Admin bearer token. Requested
  permissions are now filtered through what the caller was actually granted.

### Execution Engine

- `fix` — **`VaultAction::PayFee` bills and caps its stealth verification.** The canonical
  `stealth_transfer` path pre-charged bulletproof and balance-proof verification and enforced
  `max_fee_intent_transfers`; `PayFee` reached the same verification doing neither, so a template could
  loop `vault.pay_fee` against an unfunded vault and run full ZK verification on every validator in the
  committee, free and uncapped.
- `fix` — **Address allocations are scoped to the owning call frame.** They were tracked in one
  transaction-wide map keyed by small sequential integers and checked only for existence. A template a
  victim contract called could brute-force `GetAddress(0..k)`, learn the victim's expected future
  component address and create its own substate there first. Allocations now travel across frames the way
  buckets and proofs already do.
- `fix` — **A refused engine call fails the transaction.** The entrypoint can only answer WASM with a null
  pointer, and a template is free to ignore it, so refusals must be recorded out of band — three of the
  five null paths did not, and the transaction committed with the call's effect never applied. Version
  skew (a template calling an op an older engine cannot map) was the realistic route in.
- `fix` — The engine's own error log is no longer emitted through the metered `emit_log`, so the payer is
  not charged a `RuntimeCall` plus byte rate and a `max_logs` slot for the engine's diagnostic.
- `feat` — **Dispatcher decode panics are rendered from the template definition.** Templates emit a 5-byte
  marker and the engine expands it from the `FunctionDef` it is already invoking: `account` drops 6,691
  bytes (−2.4%). Non-breaking in both directions — an unmarked or unrenderable message passes through
  untouched.
- `perf` — **`template_lib` sheds the `EngineOp` `Debug` table and its last prose panics** (253-byte string
  table plus its match, eight `expect` messages, and a formatted null branch): another −651 bytes on
  `account`, −424 on `state`, −215 on `hello_world`.

### Indexer

- `feat` — **Optional retention window for submitted transactions.** `transaction_retention` (seconds, unset
  by default, so existing deployments are unchanged) and `transaction_prune_interval` (default 3600).
  Only the submitted transaction body and its locally recorded rejection reason are pruned — synced
  receipts are keyed independently and never touched. Pruning is by age alone, including still-pending
  transactions, since never-sequenced spam is the growth this targets. Deletes run in bounded batches so
  SQLite's database-wide write lock is not held over a large backlog.

### Docs & CI

- `docs` — New reference page on reducing template size; the stealth guide covers script-path spends
  (TIP-0006).
- `test` — Cucumber reports which node failed to start, and integration tests run on GitHub-hosted runners.
- `ci` — `cargo machete` runs on a hosted runner with a prebuilt binary instead of holding a self-hosted
  slot to compile a check that takes 0.6s.

## [0.39.1](https://github.com/tari-project/tari-ootle/compare/v0.39.0...v0.39.1) (2026-08-20)

### Release tooling

- `fix` — **`publish_crates.py` had `ootle_serde` in the wrong position**, after a crate that now depends on
  it, so a release run would fail partway through. It moves to just after `tari_bor`, and `check_order()`
  now cross-references the hand-maintained `CRATES` list against `cargo metadata` and aborts before
  anything is uploaded. `ootle_serde`'s versioned dev-dependency on `tari_template_lib` — a cycle that
  aborts packaging — is replaced by a local fixture.

## [0.39.0](https://github.com/tari-project/tari-ootle/compare/v0.38.0...v0.39.0) (2026-08-20)

### ⚠️ Upgrade notes

- `breaking` — **Testnet reset required.** Transaction ids, `TransactionReceipt`, `max_epoch`, the CBOR 128-bit
  encoding and the fee tables all changed — existing transactions are not compatible.
- `breaking` — **Templates must be rebuilt and republished against the latest `tari_template_lib`.** `Amount`'s
  wire format is now minicbor's native integer encoding (compact up to `u64::MAX`, bignum above)
  instead of a two-element digit array. It's smaller, but not backwards compatible — a template built
  on the old lib can't decode an `Amount` from the engine, or produce one it can read.
- `feat!` — **Manifests: the `info!`/`debug!`/`warn!`/`error!` macros are gone** (`Instruction::EmitLog` was
  removed). Logging *inside* a template is unchanged.
- `fix!` — **JS clients no longer patch `BigInt.prototype.toJSON`.** All bigints now serialize as strings,
  consistently — previously small values went out as numbers.

### Wallet 

- `feat!` — **`max_epoch` is now mandatory** on every transaction. `Transaction::builder(network, max_epoch)`
  takes it at construction; the network caps the window at 2160 epochs (~30 days). This change ensures 
	transactions have bounded validity.
- `feat!` — **Stealth transfers can pay up to 16 recipients in one statement** (was 8), which is cheaper than
  splitting across statements.
- `fix` — **Stealth fee estimates now settle before they're reported.** The estimate is priced from the
  transfer's actual shape locally, instead of dry-running at a guessed fee — no more estimates that
  describe a cheaper transaction than the one that gets built, and no extra network round trips.
- `fix` — **`final_fee` reports what was actually paid** (`total_fees_paid`), not the dry-run minimum. Partly
  paid transactions no longer look like they were overcharged.
- `fix` — **NFT transfers**: the wallet waits for local NFT records to update before answering, so a sent NFT
  disappears from your NFT list immediately; the web UI send dialog now stays on its result step
  instead of snapping back to the form.
- `feat` — **Web UI**: indexer liveness pill in the app bar (colour + tooltip with URL/epoch/error).
- `fix` — **Web UI**: the login screen no longer issues authenticated RPCs.
- `feat` — **ootle-wasm**: new `buildScriptPathWitness`, `buildStealthInputsStatementFromInputs` and
  `createTransferStatement` bindings — `PayTo::Conditions` outputs (hashlock/timelock/covenant MAST
  trees) can now be spent from the browser.

### Indexer

- `feat` — **Blob references are validated at ingress**, so malformed blob lists are rejected before signature
  verification instead of being forwarded to committees.
- `fix!` — `tari_indexer_client` / `@tari-project/indexer-client` pick up the bigint-as-string encoding.

### Consensus

- `feat!` — **Transactions have a bounded lifetime.** `max_transaction_validity_epochs = 2160` on every network;
  a transaction can no longer stay sequenceable forever.
- `feat!` — **Transaction ids exclude the seal signature's witness data**, so re-sealing an identical body can't
  produce N distinct valid transactions (no approve-once-execute-many).
- `feat!` — **`TransactionReceipt` gains a 32-byte intent commitment** — you can prove a transaction
  produced a given receipt without revealing the signers' or sealer's public keys.
- `fix!` — The exhaust burn rate is now bounded at build time, so a network can't be configured above
  the rate the fee estimate assumes.
- `chore!` — 128-bit CBOR integers use minicbor's native encoding (wire-breaking for values inside the
  CBOR integer range).

### Execution Engine

- `feat!` — **Publish fees retuned.** Free allowance 30 KiB → 96 KiB plus a flat 250,000 µT per publish. A large
  template (~260 KiB) drops from ~5 tTARI to ~2.7 tTARI; oversized templates stay expensive.
- `fix!` — **Receipts no longer carry `logs`**; use events for anything you need to index.
- `fix!` — **Receipts are now paid for.** The receipt substate is now charged as storage.
- `fix!` — **A log is charged for the bytes it carries**, not a flat per-call fee. Ordinary diagnostics
  cost about what they did; filling `max_logs` with 32 KiB entries no longer does.
- `fix!` — **Finalization fees are charged against the state actually persisted.** A transaction that commits
  only its fee intent is no longer priced against state that gets thrown away.
- `fix!` — **Compute is funded from the payment's unspent balance**, and the fee intent's compute is capped at a
  flat credit — free compute from repeated fee-intent aborts is closed.
- `fix!` — **Confidential accounting**: `total_supply` now tracks confidential commitments (previously
  only the revealed amount, so burnt value was reported forever), and the ElGamal value proof is sound
  (it was forgeable for any claimed value).
- `fix!` — **Engine calls are refused outside a template invocation** — from `tari_alloc`, `tari_free` or the
  response allocation. Nothing legitimate does this; it was a route to unmetered effects and, via
  `tari_alloc`, a node crash.

## [0.3.0](https://github.com/tari-project/tari-ootle/compare/v0.2.0...v0.3.0) (2023-12-19)

### ⚠ BREAKING CHANGES

* libp2p (#827)

### Features

* add version to template
  WASMs ([#835](https://github.com/tari-project/tari-ootle/issues/835)) ([8612eab](https://github.com/tari-project/tari-ootle/commit/8612eab9a1e6a713b04f86e624c5501fcf1c1808))
* do fee estimation in UI
  transfer ([#826](https://github.com/tari-project/tari-ootle/issues/826)) ([93bfd45](https://github.com/tari-project/tari-ootle/commit/93bfd452bd33fe8138d98df164bddbe7642ed650))
*
libp2p ([#827](https://github.com/tari-project/tari-ootle/issues/827)) ([9c29995](https://github.com/tari-project/tari-ootle/commit/9c29995cf0e3f5e7bbb875ea20e02dfa20eab540))
* **p2p:** peer-sync
  protocol ([#844](https://github.com/tari-project/tari-ootle/issues/844)) ([b49af42](https://github.com/tari-project/tari-ootle/commit/b49af421ec3cb72af6df42a952e26eeb4c286c03))
* request foreign
  blocks ([#760](https://github.com/tari-project/tari-ootle/issues/760)) ([7a59c4d](https://github.com/tari-project/tari-ootle/commit/7a59c4d4d2f3d3dcf55880e9a3fd12a5a73dc25e))
* show dummy blocks in
  ui ([#843](https://github.com/tari-project/tari-ootle/issues/843)) ([d5c77f6](https://github.com/tari-project/tari-ootle/commit/d5c77f6e2dbcaa9518343bc453df77c56924e219))

### Bug Fixes

* claim burn in the
  ui ([#841](https://github.com/tari-project/tari-ootle/issues/841)) ([ca80982](https://github.com/tari-project/tari-ootle/commit/ca80982672e4849f52ee5befca8e5e2e7106a003))
* cli argument
  duplicate ([#837](https://github.com/tari-project/tari-ootle/issues/837)) ([cb2d694](https://github.com/tari-project/tari-ootle/commit/cb2d694feb259683a0c58697b6d37d55c6a91867))
* force txs refetch on account change in
  UI ([#833](https://github.com/tari-project/tari-ootle/issues/833)) ([3e09ad5](https://github.com/tari-project/tari-ootle/commit/3e09ad5a2bb00dc4e309a9874f968cd17c34f7ed))
* **p2p/messaging:** single stream per
  connection ([#845](https://github.com/tari-project/tari-ootle/issues/845)) ([c0e09fe](https://github.com/tari-project/tari-ootle/commit/c0e09fefffaee7666c55c36025c039026109f21d))
* **swarm:** exit with error if unsupported seed
  multiaddr ([#836](https://github.com/tari-project/tari-ootle/issues/836)) ([b54bde8](https://github.com/tari-project/tari-ootle/commit/b54bde8178883a49038aa9b0ce6f57450e7184d6))

## [0.2.0](https://github.com/tari-project/tari-ootle/compare/v0.1.1...v0.2.0) (2023-12-08)

### ⚠ BREAKING CHANGES

* foreign broadcast reliability counter (#757)

### Features

* add transaction json download to
  ui ([#815](https://github.com/tari-project/tari-ootle/issues/815)) ([50c0ff5](https://github.com/tari-project/tari-ootle/commit/50c0ff5e5bacbcc2deb221b0cd55f42f61174551))
* disable buttons on send, add result
  dialog ([#813](https://github.com/tari-project/tari-ootle/issues/813)) ([1d146b8](https://github.com/tari-project/tari-ootle/commit/1d146b8190696b58dab6dbdae6abe8132319ea97))
* foreign broadcast reliability
  counter ([#757](https://github.com/tari-project/tari-ootle/issues/757)) ([f0dc999](https://github.com/tari-project/tari-ootle/commit/f0dc99954f634a8ac995a65bf06837edacede808))
* foreign proposal
  command ([#792](https://github.com/tari-project/tari-ootle/issues/792)) ([186b20d](https://github.com/tari-project/tari-ootle/commit/186b20d338cd3ee2c152037a6f4ba806148e44eb))
* **integration_tests:** new test for downed
  substates ([#798](https://github.com/tari-project/tari-ootle/issues/798)) ([5a0c47a](https://github.com/tari-project/tari-ootle/commit/5a0c47af80c5690869be218afdb1415742be4317))
* proper transaction signature and
  validation ([#791](https://github.com/tari-project/tari-ootle/issues/791)) ([e6a1082](https://github.com/tari-project/tari-ootle/commit/e6a108215c6e88a1e79738914aa89489836faf9f))
* set refresh balance interval to 5
  sec ([#819](https://github.com/tari-project/tari-ootle/issues/819)) ([61dfa4d](https://github.com/tari-project/tari-ootle/commit/61dfa4d996854910712b050970fdbc5c18496942))
* show substate version in dan wallet
  ui ([#810](https://github.com/tari-project/tari-ootle/issues/810)) ([89b2879](https://github.com/tari-project/tari-ootle/commit/89b287987109b26da70eed596185145d9f4afe24))
* sort TXs in UI, add
  timestamp ([#804](https://github.com/tari-project/tari-ootle/issues/804)) ([7dad32e](https://github.com/tari-project/tari-ootle/commit/7dad32ec1e8cac548b88d1d0bd4e4fe41d0db89a))

### Bug Fixes

* indexer settings in dan wallet
  ui ([#805](https://github.com/tari-project/tari-ootle/issues/805)) ([068d1ad](https://github.com/tari-project/tari-ootle/commit/068d1ad1a3cd4b9eb1a378694dc9714febca1b85))
*
propagation ([#799](https://github.com/tari-project/tari-ootle/issues/799)) ([ef10627](https://github.com/tari-project/tari-ootle/commit/ef10627ea77af78d9c4799dd115b164f2507e942))
* shard range
  computation ([#796](https://github.com/tari-project/tari-ootle/issues/796)) ([892fe0c](https://github.com/tari-project/tari-ootle/commit/892fe0ce871e6c1a8a9f70d9c51ec196f86cd175))
* shorten string on small
  strings ([#823](https://github.com/tari-project/tari-ootle/issues/823)) ([064c540](https://github.com/tari-project/tari-ootle/commit/064c54067ce09b798022bda2e0bdcbbe7a31bb8e))
* **wallet_daemon_web_ui:** send correct max_fee param on
  transfers ([#795](https://github.com/tari-project/tari-ootle/issues/795)) ([0f07b81](https://github.com/tari-project/tari-ootle/commit/0f07b8161ce6493d76d549fc2fd1b8dd9d38dfd2))
