//! # Randomness Pallet
//!
//! This pallet provides access to randomness using as source the relay chain BABE one epoch ago randomness,
//! produced by the relay chain per relay chain epoch
//!
//! There are no extrinsics for this pallet. Instead, an inherent updates the pseudo-random word obtained from
//! the relay chain when an epoch changes, and that word can be then used by other pallets as a source of randomness
//! as this pallet implements the Randomness trait
//!
//! ## Babe Epoch Randomness
//! Babe epoch randomness is retrieved once every relay chain epoch.
//!
//! The `set_babe_randomness` mandatory inherent reads the Babe epoch randomness from the
//! relay chain state proof and updates the latest pseudo-random word with this epoch randomness.
//!
//! `Config::BabeDataGetter` is responsible for reading the epoch index and epoch randomness
//! from the relay chain state proof
#![cfg_attr(not(feature = "std"), no_std)]

use frame_support::pallet;
pub use pallet::*;

#[cfg(test)]
mod mock;
#[cfg(test)]
mod tests;

/// Read babe randomness info from the relay chain state proof
pub trait GetBabeData<EpochIndex, Randomness> {
    fn get_epoch_index() -> EpochIndex;
    fn get_epoch_randomness() -> Randomness;
}

#[pallet]
pub mod pallet {
    use super::*;
    use frame_support::pallet_prelude::*;
    use frame_system::pallet_prelude::{BlockNumberFor, *};
    use frame_system::WeightInfo;
    use scale_info::prelude::vec::Vec;
    use session_keys_primitives::{InherentError, INHERENT_IDENTIFIER};
    use sp_runtime::traits::{Hash, Saturating};

    #[pallet::pallet]
    pub struct Pallet<T>(PhantomData<T>);

    /// Configuration trait of this pallet.
    #[pallet::config]
    pub trait Config: frame_system::Config {
        /// Overarching event type
        type RuntimeEvent: From<Event<Self>> + IsType<<Self as frame_system::Config>::RuntimeEvent>;

        /// Get the BABE data from the runtime
        type BabeDataGetter: GetBabeData<u64, Option<Self::Hash>>;

        /// Weight info
        type WeightInfo: WeightInfo;
    }

    #[pallet::event]
    #[pallet::generate_deposit(pub(crate) fn deposit_event)]
    pub enum Event<T: Config> {
        /// Event emitted when a new random seed is available from the relay chain
        NewRandomnessAvailable {
            randomness_seed: T::Hash,
            from_epoch: u64,
            valid_until_block: BlockNumberFor<T>,
        },
    }

    /// Latest random seed obtained from BABE and the latest block that it can process randomness requests from
    #[pallet::storage]
    pub type LatestBabeRandomness<T: Config> = StorageValue<_, (T::Hash, BlockNumberFor<T>)>;

    /// Current relay epoch
    #[pallet::storage]
    pub(crate) type RelayEpoch<T: Config> = StorageValue<_, u64, ValueQuery>;

    /// Ensures the mandatory inherent was included in the block
    #[pallet::storage]
    pub(crate) type InherentIncluded<T: Config> = StorageValue<_, ()>;

    #[pallet::call]
    impl<T: Config> Pallet<T> {
        /// This inherent that must be included (DispatchClass::Mandatory) at each block saves the latest randomness available from the
        /// relay chain into a variable that can then be used as a seed for commitments that happened during
        /// the previous relay chain epoch
        #[pallet::call_index(0)]
        #[pallet::weight((
			Weight::from_parts(10_000, 0) + T::DbWeight::get().writes(1),
			DispatchClass::Mandatory
		))]
        pub fn set_babe_randomness(origin: OriginFor<T>) -> DispatchResultWithPostInfo {
            // Make sure this is included in the block as an inherent, unsigned
            ensure_none(origin)?;

            // Get the last relay epoch index for which the randomness has been processed
            let last_relay_epoch_index = <RelayEpoch<T>>::get();

            // Get the current epoch of the relay chain
            let relay_epoch_index = T::BabeDataGetter::get_epoch_index();

            // If the current epoch is greater than the one for which the randomness was last processed for
            if relay_epoch_index > last_relay_epoch_index {
                // Get the new randomness of this new epoch
                if let Some(randomness) = T::BabeDataGetter::get_epoch_randomness() {
                    // The latest BABE randomness is predictable during the current epoch and this inherent
                    // must be executed and included in every block, which means that iff this logic is being
                    // executed, the epoch JUST changed, so the obtained randomness is valid for every previous block.
                    // TODO: add logic to check parent relay block (ideally, we make it valid for `curr_relay_block - 2`)
                    let latest_valid_block = frame_system::Pallet::<T>::block_number()
                        .saturating_sub(sp_runtime::traits::One::one());

                    // Save it to be readily available for use
                    LatestBabeRandomness::<T>::put((randomness, latest_valid_block));

                    // Update storage with the latest epoch for which randomness was processed for
                    <RelayEpoch<T>>::put(relay_epoch_index);

                    // Emit an event detailing that a new randomness is available
                    Self::deposit_event(Event::NewRandomnessAvailable {
                        randomness_seed: randomness,
                        from_epoch: relay_epoch_index,
                        valid_until_block: latest_valid_block,
                    });
                } else {
                    log::warn!(
                        "Failed to fill BABE epoch randomness \
							REQUIRE HOTFIX TO FILL EPOCH RANDOMNESS FOR EPOCH {:?}",
                        relay_epoch_index
                    );
                }
            }

            // Update storage to reflect that this inherent was included in the block (so the block is valid)
            <InherentIncluded<T>>::put(());

            // Inherents do not pay for execution
            Ok(Pays::No.into())
        }
    }

    /// Implement the required trait to provide an inherent to the runtime
    #[pallet::inherent]
    impl<T: Config> ProvideInherent for Pallet<T> {
        type Call = Call<T>;
        type Error = InherentError;
        const INHERENT_IDENTIFIER: InherentIdentifier = INHERENT_IDENTIFIER;

        // This function returns if the inherent should be added to the current block or not
        fn is_inherent_required(_: &InherentData) -> Result<Option<Self::Error>, Self::Error> {
            // Return Ok(Some(_)) unconditionally because this inherent is required in every block
            // If it is not found, throw a InherentRequired error.
            Ok(Some(InherentError::Other(
                sp_runtime::RuntimeString::Borrowed("Inherent required to set babe randomness"),
            )))
        }

        // The empty-payload inherent extrinsic.
        fn create_inherent(_data: &InherentData) -> Option<Self::Call> {
            Some(Call::set_babe_randomness {})
        }

        fn is_inherent(call: &Self::Call) -> bool {
            matches!(call, Call::set_babe_randomness { .. })
        }
    }

    #[pallet::hooks]
    impl<T: Config> Hooks<BlockNumberFor<T>> for Pallet<T> {
        /// This hook is called on block initialization and returns the Weight of the `on_finalize` hook to
        /// let block builders know how much weight to reserve for it
        /// TODO: Benchmark on_finalize to get its weight and replace the placeholder weight for that
        fn on_initialize(_now: BlockNumberFor<T>) -> Weight {
            Weight::from_parts(10_000, 0) + T::DbWeight::get().reads_writes(1, 1)
        }
        /// This hook checks, on block finalization, that the required inherent was included and clears
        /// storage to make it necessary to include it in future blocks as well
        fn on_finalize(_now: BlockNumberFor<T>) {
            // Ensure the mandatory inherent was included in the block or the block is invalid
            // We use take() to make sure this is storage is not set for the next block
            assert!(
				<InherentIncluded<T>>::take().is_some(),
				"Mandatory randomness inherent not included; InherentIncluded storage item is empty"
			);
        }
    }

    // Randomness trait
    impl<T: Config> frame_support::traits::Randomness<T::Hash, BlockNumberFor<T>> for Pallet<T> {
        /// Uses the BABE randomness of this epoch to generate a random seed that can be used
        /// for commitments from the last epoch. The provided `subject` MUST have been committed
        /// AT LEAST during the last epoch for the result of this function to not be predictable
        ///
        /// The subject is a byte array that is hashed (to make it a fixed size) and then concatenated with
        /// the latest BABE randomness. The result is then hashed again to provide the final randomness.
        fn random(subject: &[u8]) -> (T::Hash, BlockNumberFor<T>) {
            // If there's randomness available
            if let Some((babe_randomness, latest_valid_block)) = LatestBabeRandomness::<T>::get() {
                let hashed_subject = T::Hashing::hash(subject);
                let mut digest = Vec::new();
                // Concatenate the latest randomness with the hashed subject
                digest.extend_from_slice(babe_randomness.as_ref());
                digest.extend_from_slice(hashed_subject.as_ref());
                // Hash it
                let randomness = T::Hashing::hash(digest.as_slice());
                // Return the randomness for this subject and the latest block for which this randomness is useful
                // `subject` commitments done after `latest_valid_block` are predictable, and as such MUST be discarded
                (randomness, latest_valid_block)
            } else {
                // If there's no randomness available, return an empty randomness that's invalid for every block
                let randomness = T::Hash::default();
                let latest_valid_block: BlockNumberFor<T> = sp_runtime::traits::Zero::zero();
                (randomness, latest_valid_block)
            }
        }
    }

    // Read-only functions
    impl<T: Config> Pallet<T> {
        /// Get the latest BABE randomness seed and the latest block for which it's valid
        pub fn latest_babe_randomness() -> Option<(T::Hash, BlockNumberFor<T>)> {
            LatestBabeRandomness::<T>::get()
        }

        /// Get the latest relay epoch processed
        pub fn relay_epoch() -> u64 {
            RelayEpoch::<T>::get()
        }

        /// Get the variable that's used to check if the mandatory BABE inherent was included in the block
        pub fn inherent_included() -> Option<()> {
            InherentIncluded::<T>::get()
        }
    }
}
