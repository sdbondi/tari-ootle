//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

//! Recomputes each streamed shard's transition accumulator from the versions it is streamed, and checks it against
//! quorum-verified shard leaves.
//!
//! The accumulator is cumulative, so a leaf that matches the accumulator computed at its version authenticates every
//! version streamed up to it.

use std::collections::{HashMap, VecDeque};

use ootle_network::Network;
use tari_engine_types::{published_template::PublishedTemplateAddress, substate::hash_published_template_metadata};
use tari_ootle_common_types::{Epoch, StateVersion, shard::Shard};
use tari_ootle_storage::consensus_models::{EpochCheckpoint, SubstateUpdateProof};
use tari_state_tree::{StateTreeError, SubstateTreeChange, TransitionAccumulator};
use tari_template_lib_types::Hash32;

/// The most versions a shard keeps the accumulator of, oldest dropped first, for a leaf that trails the stream. A tip
/// leaf comes from a committee member probed in the last few seconds, so it trails the stream by a handful of
/// versions.
const MAX_ACCUMULATOR_HISTORY: usize = 256;

/// A shard's state version and transition accumulator as a quorum committed them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifiedShardLeaf {
    pub shard: Shard,
    pub state_version: StateVersion,
    pub transition_acc: TransitionAccumulator,
}

impl VerifiedShardLeaf {
    /// The leaves of every shard `checkpoint` summarises but the global shard. Every shard group commits its own copy
    /// of the global shard, and the stream follows only one of them.
    pub fn from_checkpoint(checkpoint: &EpochCheckpoint) -> impl Iterator<Item = Self> + '_ {
        checkpoint
            .shard_tree_summary()
            .iter()
            .filter(|(shard, _)| !shard.is_global())
            .map(|(shard, summary)| Self {
                shard: *shard,
                state_version: StateVersion::new(summary.state_version),
                transition_acc: summary.transition_acc,
            })
    }
}

/// The outcome of checking a shard's computed accumulator against a verified leaf. `P` names who streamed a version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict<P> {
    /// The accumulator computed at the version equals the leaf's, which authenticates every version streamed up to
    /// it.
    Promote(StateVersion),
    /// The shard is tracked from the leaf's version on, starting from the leaf's accumulator. The versions up to it
    /// were streamed without every transition, so the leaf stands in for them.
    Adopt(StateVersion),
    /// The accumulator computed at `version` differs from the leaf's. The shard is tracked again from a later leaf.
    Mismatch {
        version: StateVersion,
        epoch: Epoch,
        computed: TransitionAccumulator,
        verified: TransitionAccumulator,
        streamed_by: P,
    },
    /// The leaf disagrees with the accumulator an earlier verified leaf set at the same version. A quorum signed
    /// both, so the fault lies with the signers or with this indexer's leaf verification.
    Conflict {
        version: StateVersion,
        held: TransitionAccumulator,
        verified: TransitionAccumulator,
    },
    /// The stream has not reached the leaf's version. The leaf is checked once it does.
    Wait,
    /// The leaf's version is below every accumulator the shard holds.
    Stale,
}

/// The transition accumulator of every shard the indexer streams, checked against verified leaves.
#[derive(Debug)]
pub struct VerifiedStage<P> {
    shards: HashMap<Shard, ShardStage<P>>,
}

impl<P> Default for VerifiedStage<P> {
    fn default() -> Self {
        Self { shards: HashMap::new() }
    }
}

impl<P: Clone> VerifiedStage<P> {
    /// Extends `shard`'s accumulator with `version`, committed at `epoch` with `changes` and streamed by
    /// `streamed_by`, and checks a leaf that was waiting for it.
    ///
    /// A version that does not follow the last one streamed to the stage leaves the shard untracked until a leaf names
    /// its accumulator. So does a version whose transitions no committed version could hold, which is an error that
    /// carries the verdict on a leaf waiting for it.
    pub fn on_version(
        &mut self,
        shard: Shard,
        version: StateVersion,
        epoch: Epoch,
        changes: &[SubstateTreeChange],
        streamed_by: P,
    ) -> Result<Option<Verdict<P>>, UncomputableVersion<P>> {
        let stage = self.shards.entry(shard).or_default();
        let previous = match stage.head {
            Head::Known(head, acc) if head.as_u64().checked_add(1) == Some(version.as_u64()) => Some(acc),
            Head::Unseen if version.as_u64() == 1 => Some(TransitionAccumulator::zero()),
            _ => None,
        };
        let Some(previous) = previous else {
            return Ok(stage.lose_track(version));
        };
        let acc = match previous.extend(epoch, version.as_u64(), changes) {
            Ok(acc) => acc,
            Err(source) => {
                return Err(UncomputableVersion {
                    source,
                    verdict: stage.lose_track(version),
                });
            },
        };
        stage.head = Head::Known(version, acc);
        stage.history.push_back(Computed {
            version,
            epoch,
            acc,
            streamed_by,
        });
        if stage.history.len() > MAX_ACCUMULATOR_HISTORY {
            stage.history.pop_front();
        }
        Ok(stage.check_waiting())
    }

    /// Records that `shard` was streamed up to `version` without every transition, so its accumulator is unknown until
    /// a leaf names it.
    pub fn on_untracked_version(&mut self, shard: Shard, version: StateVersion) -> Option<Verdict<P>> {
        self.shards.entry(shard).or_default().lose_track(version)
    }

    pub fn on_leaf(&mut self, leaf: VerifiedShardLeaf) -> Verdict<P> {
        self.shards.entry(leaf.shard).or_default().check(leaf)
    }

    /// Checks the leaves of `checkpoint` (see [`VerifiedShardLeaf::from_checkpoint`]) that are of a streamed shard.
    pub fn on_checkpoint(&mut self, checkpoint: &EpochCheckpoint) -> Vec<(Shard, Verdict<P>)> {
        VerifiedShardLeaf::from_checkpoint(checkpoint)
            .filter_map(|leaf| {
                let stage = self.shards.get_mut(&leaf.shard)?;
                Some((leaf.shard, stage.check(leaf)))
            })
            .collect()
    }

    /// Whether a leaf of `shard` would check or start tracking anything: versions were streamed above its last
    /// verified one, or it is untracked, and no leaf is already waiting for the stream.
    pub fn wants_leaf(&self, shard: Shard) -> bool {
        self.shards.get(&shard).is_some_and(ShardStage::wants_leaf)
    }
}

#[derive(Debug)]
struct ShardStage<P> {
    head: Head,
    /// The accumulator after each version from the last verified one to the head, oldest first.
    history: VecDeque<Computed<P>>,
    verified: Option<(StateVersion, TransitionAccumulator)>,
    /// The lowest leaf ahead of the stream.
    waiting: Option<VerifiedShardLeaf>,
}

impl<P> Default for ShardStage<P> {
    fn default() -> Self {
        Self {
            head: Head::Unseen,
            history: VecDeque::new(),
            verified: None,
            waiting: None,
        }
    }
}

#[derive(Debug)]
struct Computed<P> {
    version: StateVersion,
    epoch: Epoch,
    acc: TransitionAccumulator,
    streamed_by: P,
}

#[derive(Debug, Clone, Copy)]
enum Head {
    /// Nothing has been streamed to the stage.
    Unseen,
    /// The last version streamed, whose accumulator is unknown.
    Unknown(StateVersion),
    /// The last version streamed and the accumulator after it.
    Known(StateVersion, TransitionAccumulator),
}

impl<P: Clone> ShardStage<P> {
    fn check(&mut self, leaf: VerifiedShardLeaf) -> Verdict<P> {
        match self.head {
            Head::Unseen => self.wait(leaf),
            Head::Unknown(head) if leaf.state_version == head => {
                self.head = Head::Known(head, leaf.transition_acc);
                self.history.clear();
                self.verified = Some((head, leaf.transition_acc));
                Verdict::Adopt(head)
            },
            Head::Unknown(head) | Head::Known(head, _) if leaf.state_version > head => self.wait(leaf),
            Head::Unknown(_) => Verdict::Stale,
            Head::Known(head, _) => {
                if let Some((version, held)) = self.verified &&
                    version == leaf.state_version
                {
                    return if held == leaf.transition_acc {
                        Verdict::Promote(version)
                    } else {
                        Verdict::Conflict {
                            version,
                            held,
                            verified: leaf.transition_acc,
                        }
                    };
                }
                let Some(position) = self
                    .history
                    .iter()
                    .position(|computed| computed.version == leaf.state_version)
                else {
                    return Verdict::Stale;
                };
                let computed = &self.history[position];
                if computed.acc != leaf.transition_acc {
                    let verdict = Verdict::Mismatch {
                        version: computed.version,
                        epoch: computed.epoch,
                        computed: computed.acc,
                        verified: leaf.transition_acc,
                        streamed_by: computed.streamed_by.clone(),
                    };
                    self.head = Head::Unknown(head);
                    self.history.clear();
                    return verdict;
                }
                self.history.drain(..=position);
                self.verified = Some((leaf.state_version, leaf.transition_acc));
                Verdict::Promote(leaf.state_version)
            },
        }
    }

    fn wait(&mut self, leaf: VerifiedShardLeaf) -> Verdict<P> {
        if self
            .waiting
            .is_none_or(|waiting| leaf.state_version < waiting.state_version)
        {
            self.waiting = Some(leaf);
        }
        Verdict::Wait
    }

    fn check_waiting(&mut self) -> Option<Verdict<P>> {
        let head = match self.head {
            Head::Unseen => return None,
            Head::Unknown(head) | Head::Known(head, _) => head,
        };
        let leaf = self.waiting.take_if(|leaf| leaf.state_version <= head)?;
        Some(self.check(leaf))
    }

    fn lose_track(&mut self, version: StateVersion) -> Option<Verdict<P>> {
        self.head = Head::Unknown(version);
        self.history.clear();
        self.check_waiting()
    }

    fn wants_leaf(&self) -> bool {
        if self.waiting.is_some() {
            return false;
        }
        match self.head {
            Head::Unseen => false,
            Head::Unknown(_) => true,
            Head::Known(head, _) => self.verified.is_none_or(|(version, _)| version != head),
        }
    }
}

/// The tree change a streamed transition makes.
///
/// A template is streamed as its metadata and value hash. The hash is a function of the metadata, so it is derived
/// from the metadata, and a sent hash or template address that disagrees with the metadata is rejected.
pub fn streamed_tree_change(
    network: Network,
    epoch: Epoch,
    update: &SubstateUpdateProof,
) -> Result<SubstateTreeChange, StreamedTransitionError> {
    let Some(create) = update.as_create() else {
        return Ok(update.to_tree_change(network, epoch));
    };
    let (Some(metadata), Some(address)) = (
        &create.substate.template_metadata,
        create.substate.substate_id().as_template(),
    ) else {
        return Ok(update.to_tree_change(network, epoch));
    };
    if PublishedTemplateAddress::from_author_and_binary_hash(&metadata.author_public_key, &metadata.binary_hash) !=
        address
    {
        return Err(StreamedTransitionError::TemplateAddress { address });
    }
    let value_hash = hash_published_template_metadata(network, metadata, create.substate.version, epoch);
    let sent = create.substate.to_value_hash(network, epoch);
    if sent != value_hash {
        return Err(StreamedTransitionError::TemplateValueHash {
            address,
            sent,
            derived: value_hash,
        });
    }
    Ok(SubstateTreeChange::Up {
        id: create.substate.as_versioned_substate_id_ref().to_owned(),
        value_hash,
    })
}

/// A streamed version whose transitions no committed version could hold. The shard is untracked from it on.
#[derive(Debug)]
pub struct UncomputableVersion<P> {
    pub source: StateTreeError,
    /// The verdict on a leaf that was waiting for the version.
    pub verdict: Option<Verdict<P>>,
}

#[derive(Debug, thiserror::Error)]
pub enum StreamedTransitionError {
    #[error("template {address} was streamed with metadata whose author and binary hash derive another address")]
    TemplateAddress { address: PublishedTemplateAddress },
    #[error("template {address} was streamed with value hash {sent}, but its metadata hashes to {derived}")]
    TemplateValueHash {
        address: PublishedTemplateAddress,
        sent: Hash32,
        derived: Hash32,
    },
}

#[cfg(test)]
mod tests {
    use tari_engine_types::{SubstateVersion, published_template::PublishedTemplateMetadata, substate::SubstateId};
    use tari_ootle_common_types::VersionedSubstateId;
    use tari_ootle_storage::consensus_models::{SubstateCreate, SubstateData, SubstateDestroy, SubstateValueOrHash};
    use tari_template_lib_types::{ComponentAddress, ObjectKey, crypto::RistrettoPublicKeyBytes};

    use super::*;

    const EPOCH: Epoch = Epoch(3);
    const PEER: u8 = 9;

    fn shard() -> Shard {
        Shard::from(3u32)
    }

    fn v(n: u64) -> StateVersion {
        StateVersion::new(n)
    }

    fn id(seed: u64) -> VersionedSubstateId {
        let mut key = [0; ObjectKey::LENGTH];
        key[..8].copy_from_slice(&seed.to_le_bytes());
        VersionedSubstateId::new(
            SubstateId::Component(ComponentAddress::new(ObjectKey::from_array(key))),
            SubstateVersion::ZERO,
        )
    }

    fn up(seed: u64, value: u8) -> SubstateTreeChange {
        SubstateTreeChange::Up {
            id: id(seed),
            value_hash: Hash32::from_array([value; 32]),
        }
    }

    fn version_changes(version: u64) -> Vec<SubstateTreeChange> {
        vec![up(version, 1), up(version + 1000, 2)]
    }

    /// A shard's committed history: the transitions of each version from v1, and the accumulator after each.
    struct History {
        versions: Vec<Vec<SubstateTreeChange>>,
        accs: Vec<TransitionAccumulator>,
    }

    impl History {
        fn new(num_versions: u64) -> Self {
            let versions = (1..=num_versions).map(version_changes).collect::<Vec<_>>();
            let mut acc = TransitionAccumulator::zero();
            let accs = versions
                .iter()
                .zip(1..)
                .map(|(changes, n)| {
                    acc = acc.extend(EPOCH, n, changes).unwrap();
                    acc
                })
                .collect();
            Self { versions, accs }
        }

        fn changes(&self, version: u64) -> &[SubstateTreeChange] {
            &self.versions[usize::try_from(version - 1).unwrap()]
        }

        fn leaf(&self, version: u64) -> VerifiedShardLeaf {
            VerifiedShardLeaf {
                shard: shard(),
                state_version: v(version),
                transition_acc: self.accs[usize::try_from(version - 1).unwrap()],
            }
        }

        fn stream(&self, stage: &mut VerifiedStage<u8>, versions: impl IntoIterator<Item = u64>) -> Vec<Verdict<u8>> {
            versions
                .into_iter()
                .filter_map(|n| stage.on_version(shard(), v(n), EPOCH, self.changes(n), PEER).unwrap())
                .collect()
        }
    }

    #[test]
    fn a_leaf_at_the_head_promotes_every_version_streamed() {
        let history = History::new(3);
        let mut stage = VerifiedStage::default();
        assert!(history.stream(&mut stage, 1..=3).is_empty());

        assert_eq!(stage.on_leaf(history.leaf(3)), Verdict::Promote(v(3)));
        assert!(!stage.wants_leaf(shard()));
    }

    #[test]
    fn a_leaf_behind_the_stream_promotes_up_to_its_version() {
        let history = History::new(5);
        let mut stage = VerifiedStage::default();
        history.stream(&mut stage, 1..=5);

        assert_eq!(stage.on_leaf(history.leaf(3)), Verdict::Promote(v(3)));
        assert!(stage.wants_leaf(shard()));
        assert_eq!(stage.on_leaf(history.leaf(2)), Verdict::Stale);
        assert_eq!(stage.on_leaf(history.leaf(5)), Verdict::Promote(v(5)));
    }

    #[test]
    fn a_leaf_ahead_of_the_stream_is_checked_when_the_stream_reaches_it() {
        let history = History::new(4);
        let mut stage = VerifiedStage::default();
        history.stream(&mut stage, 1..=1);

        assert_eq!(stage.on_leaf(history.leaf(4)), Verdict::Wait);
        assert_eq!(stage.on_leaf(history.leaf(3)), Verdict::Wait);
        assert!(!stage.wants_leaf(shard()));

        assert_eq!(history.stream(&mut stage, 2..=4), vec![Verdict::Promote(v(3))]);
    }

    type Tamper = dyn Fn(&mut Vec<SubstateTreeChange>, &mut Epoch);

    #[test]
    fn a_history_that_differs_from_the_committed_one_mismatches_at_the_next_leaf() {
        let history = History::new(3);
        let tampered: [(&str, Box<Tamper>); 4] = [
            ("a tampered value", Box::new(|changes, _| changes[0] = up(2, 7))),
            ("an omitted transition", Box::new(|changes, _| drop(changes.pop()))),
            ("an extra transition", Box::new(|changes, _| changes.push(up(77, 1)))),
            ("a wrong epoch", Box::new(|_, epoch| *epoch = Epoch(4))),
        ];
        for (case, tamper) in tampered {
            let mut stage = VerifiedStage::default();
            history.stream(&mut stage, 1..=1);
            let mut changes = version_changes(2);
            let mut epoch = EPOCH;
            tamper(&mut changes, &mut epoch);
            stage.on_version(shard(), v(2), epoch, &changes, PEER).unwrap();
            history.stream(&mut stage, 3..=3);

            let Verdict::Mismatch {
                version,
                epoch,
                computed,
                verified,
                streamed_by,
            } = stage.on_leaf(history.leaf(3))
            else {
                panic!("{case} is not a mismatch");
            };
            assert_eq!(
                (version, epoch, verified, streamed_by),
                (v(3), EPOCH, history.accs[2], PEER),
                "{case}"
            );
            assert_ne!(computed, verified, "{case}");
            assert!(stage.wants_leaf(shard()), "{case} leaves the shard tracked");
        }
    }

    #[test]
    fn a_shard_streamed_without_every_transition_is_tracked_from_a_leaf_at_its_version() {
        let history = History::new(8);
        let mut stage = VerifiedStage::default();
        assert_eq!(stage.on_untracked_version(shard(), v(5)), None);
        assert!(stage.wants_leaf(shard()));

        assert_eq!(stage.on_leaf(history.leaf(4)), Verdict::Stale);
        assert_eq!(stage.on_leaf(history.leaf(7)), Verdict::Wait);
        assert_eq!(history.stream(&mut stage, 6..=7), vec![Verdict::Adopt(v(7))]);

        history.stream(&mut stage, 8..=8);
        assert_eq!(stage.on_leaf(history.leaf(8)), Verdict::Promote(v(8)));
    }

    #[test]
    fn a_shard_first_streamed_past_its_first_version_is_untracked() {
        let history = History::new(4);
        let mut stage = VerifiedStage::default();
        history.stream(&mut stage, 3..=3);

        assert_eq!(stage.on_leaf(history.leaf(3)), Verdict::Adopt(v(3)));
        history.stream(&mut stage, 4..=4);
        assert_eq!(stage.on_leaf(history.leaf(4)), Verdict::Promote(v(4)));
    }

    #[test]
    fn a_skipped_version_leaves_the_shard_untracked() {
        let history = History::new(4);
        let mut stage = VerifiedStage::default();
        history.stream(&mut stage, [1, 2, 4]);

        assert_eq!(stage.on_leaf(history.leaf(2)), Verdict::Stale);
        assert_eq!(stage.on_leaf(history.leaf(4)), Verdict::Adopt(v(4)));
    }

    #[test]
    fn a_version_no_block_could_commit_is_an_error_and_untracks_the_shard() {
        let history = History::new(2);
        let mut stage = VerifiedStage::default();
        history.stream(&mut stage, 1..=1);

        stage
            .on_version(shard(), v(2), EPOCH, &[up(1, 1), up(1, 2)], PEER)
            .unwrap_err();
        assert!(stage.wants_leaf(shard()));
        assert_eq!(stage.on_leaf(history.leaf(1)), Verdict::Stale);
        assert_eq!(stage.on_leaf(history.leaf(2)), Verdict::Adopt(v(2)));
    }

    #[test]
    fn a_leaf_waiting_for_a_version_no_block_could_commit_is_adopted_with_the_error() {
        let history = History::new(3);
        let mut stage = VerifiedStage::default();
        history.stream(&mut stage, 1..=1);
        assert_eq!(stage.on_leaf(history.leaf(2)), Verdict::Wait);

        let error = stage
            .on_version(shard(), v(2), EPOCH, &[up(1, 1), up(1, 2)], PEER)
            .unwrap_err();
        assert_eq!(error.verdict, Some(Verdict::Adopt(v(2))));

        history.stream(&mut stage, 3..=3);
        assert_eq!(stage.on_leaf(history.leaf(3)), Verdict::Promote(v(3)));
    }

    #[test]
    fn a_leaf_that_disagrees_with_an_earlier_verified_leaf_conflicts() {
        let history = History::new(3);
        let forged = |version| VerifiedShardLeaf {
            transition_acc: TransitionAccumulator::new([7; 32]),
            ..history.leaf(version)
        };

        let mut stage = VerifiedStage::default();
        history.stream(&mut stage, 1..=3);
        assert_eq!(stage.on_leaf(history.leaf(2)), Verdict::Promote(v(2)));
        assert_eq!(stage.on_leaf(history.leaf(2)), Verdict::Promote(v(2)));
        assert_eq!(stage.on_leaf(forged(2)), Verdict::Conflict {
            version: v(2),
            held: history.accs[1],
            verified: forged(2).transition_acc,
        });

        let mut stage = VerifiedStage::<u8>::default();
        stage.on_untracked_version(shard(), v(3));
        assert_eq!(stage.on_leaf(history.leaf(3)), Verdict::Adopt(v(3)));
        assert_eq!(stage.on_leaf(forged(3)), Verdict::Conflict {
            version: v(3),
            held: history.accs[2],
            verified: forged(3).transition_acc,
        });
    }

    #[test]
    fn a_shard_nothing_was_streamed_for_wants_no_leaf() {
        let history = History::new(1);
        let mut stage = VerifiedStage::<u8>::default();
        assert!(!stage.wants_leaf(shard()));

        assert_eq!(stage.on_leaf(history.leaf(1)), Verdict::Wait);
        assert_eq!(history.stream(&mut stage, 1..=1), vec![Verdict::Promote(v(1))]);
    }

    fn template_metadata() -> PublishedTemplateMetadata {
        PublishedTemplateMetadata {
            template_name: "Counter".to_string(),
            author_public_key: RistrettoPublicKeyBytes::default(),
            binary_hash: Hash32::from_array([5; 32]),
            at_epoch: 1,
            metadata_hash: None,
        }
    }

    fn streamed_template(
        address: PublishedTemplateAddress,
        metadata: PublishedTemplateMetadata,
        value_hash: Hash32,
    ) -> SubstateUpdateProof {
        SubstateCreate {
            substate: SubstateData {
                substate_id: SubstateId::Template(address),
                version: SubstateVersion::ZERO,
                value: SubstateValueOrHash::Hash(value_hash),
                template_metadata: Some(metadata),
            },
        }
        .into()
    }

    fn address_of(metadata: &PublishedTemplateMetadata) -> PublishedTemplateAddress {
        PublishedTemplateAddress::from_author_and_binary_hash(&metadata.author_public_key, &metadata.binary_hash)
    }

    #[test]
    fn a_template_is_hashed_from_its_metadata() {
        let metadata = template_metadata();
        let value_hash = hash_published_template_metadata(Network::LocalNet, &metadata, SubstateVersion::ZERO, EPOCH);
        let update = streamed_template(address_of(&metadata), metadata, value_hash);

        let change = streamed_tree_change(Network::LocalNet, EPOCH, &update).unwrap();
        assert!(
            matches!(&change, SubstateTreeChange::Up { id, value_hash: hash } if *id == update.to_versioned_substate_id() && *hash == value_hash)
        );
    }

    #[test]
    fn a_template_whose_metadata_disagrees_with_its_hash_or_address_is_rejected() {
        let metadata = template_metadata();
        let value_hash = hash_published_template_metadata(Network::LocalNet, &metadata, SubstateVersion::ZERO, EPOCH);
        let renamed = PublishedTemplateMetadata {
            template_name: "Renamed".to_string(),
            ..metadata.clone()
        };
        let update = streamed_template(address_of(&metadata), renamed, value_hash);
        assert!(matches!(
            streamed_tree_change(Network::LocalNet, EPOCH, &update),
            Err(StreamedTransitionError::TemplateValueHash { .. })
        ));

        let other_binary = PublishedTemplateMetadata {
            binary_hash: Hash32::from_array([6; 32]),
            ..metadata.clone()
        };
        let update = streamed_template(address_of(&metadata), other_binary, value_hash);
        assert!(matches!(
            streamed_tree_change(Network::LocalNet, EPOCH, &update),
            Err(StreamedTransitionError::TemplateAddress { .. })
        ));
    }

    #[test]
    fn a_destroyed_substate_is_a_down() {
        let id = id(1);
        let update = SubstateUpdateProof::Destroy(SubstateDestroy {
            substate_id: id.substate_id().clone(),
            version: id.version(),
        });
        let change = streamed_tree_change(Network::LocalNet, EPOCH, &update).unwrap();
        assert!(matches!(change, SubstateTreeChange::Down { id: down } if down == id));
    }
}
