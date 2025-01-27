use std::str::FromStr;

use log::{debug, error, info};
use sp_core::H256;
use storage_hub_infra::{actor::ActorHandle, event_bus::EventHandler};

use crate::services::{
    blockchain::{
        commands::BlockchainServiceInterface, events::NewStorageRequest,
        handler::BlockchainService, types::ExtrinsicResult,
    },
    StorageHubHandler, StorageHubHandlerConfig,
};

const LOG_TARGET: &str = "bsp-volunteer-mock-task";

pub struct BspVolunteerMockTask<SHC: StorageHubHandlerConfig> {
    storage_hub_handler: StorageHubHandler<SHC>,
}

impl<SHC: StorageHubHandlerConfig> Clone for BspVolunteerMockTask<SHC> {
    fn clone(&self) -> BspVolunteerMockTask<SHC> {
        Self {
            storage_hub_handler: self.storage_hub_handler.clone(),
        }
    }
}

impl<SHC: StorageHubHandlerConfig> BspVolunteerMockTask<SHC> {
    pub fn new(storage_hub_handler: StorageHubHandler<SHC>) -> Self {
        Self {
            storage_hub_handler,
        }
    }
}

impl<SHC: StorageHubHandlerConfig> EventHandler<NewStorageRequest> for BspVolunteerMockTask<SHC> {
    async fn handle_event(&self, event: NewStorageRequest) -> anyhow::Result<()> {
        info!(
            target: LOG_TARGET,
            "Initiating BSP volunteer mock for location: {:?}, fingerprint: {:?}",
            event.location,
            event.fingerprint
        );

        // Build extrinsic.
        let call =
            storage_hub_runtime::RuntimeCall::FileSystem(pallet_file_system::Call::bsp_volunteer {
                location: event.location,
                fingerprint: event.fingerprint,
            });

        let (mut tx_watcher, tx_hash) = self
            .storage_hub_handler
            .blockchain
            .send_extrinsic(call)
            .await?;

        // Wait for the transaction to be included in a block.
        let mut block_hash = None;
        // TODO: Consider adding a timeout.
        while let Some(tx_result) = tx_watcher.recv().await {
            // Parse the JSONRPC string, now that we know it is not an error.
            let json: serde_json::Value = serde_json::from_str(&tx_result)
                .expect("The result, if not an error, can only be a JSONRPC string; qed");

            debug!(target: LOG_TARGET, "Transaction information: {:?}", json);

            // Checking if the transaction is included in a block.
            // TODO: Consider if we might want to wait for "finalized".
            // TODO: Handle other lifetime extrinsic edge cases. See https://github.com/paritytech/polkadot-sdk/blob/master/substrate/client/transaction-pool/api/src/lib.rs#L131
            if let Some(in_block) = json["params"]["result"]["inBlock"].as_str() {
                block_hash = Some(H256::from_str(in_block)?);
                let subscription_id = json["params"]["subscription"]
                    .as_number()
                    .expect("Subscription should exist and be a number; qed");

                // Unwatch extrinsic to release tx_watcher.
                self.storage_hub_handler
                    .blockchain
                    .unwatch_extrinsic(subscription_id.to_owned())
                    .await?;

                // Breaking while loop.
                // Even though we unwatch the transaction, and the loop should break, we still break manually
                // in case we continue to receive updates. This should not happen, but it is a safety measure,
                // and we already have what we need.
                break;
            }
        }

        // Get the extrinsic from the block, with its events.
        let block_hash = block_hash.expect(
            "Block hash should exist after waiting for extrinsic to be included in a block; qed",
        );
        let extrinsic_in_block = self
            .storage_hub_handler
            .blockchain
            .get_extrinsic_from_block(block_hash, tx_hash)
            .await?;

        // Check if the extrinsic was successful. In this mocked task we know this should fail if Alice is
        // not a registered BSP.
        let extrinsic_successful = ActorHandle::<BlockchainService>::extrinsic_result(extrinsic_in_block.clone())
            .expect("Extrinsic does not contain an ExtrinsicFailed nor ExtrinsicSuccess event, which is not possible; qed");
        match extrinsic_successful {
            ExtrinsicResult::Success { dispatch_info } => {
                info!(target: LOG_TARGET, "Extrinsic successful with dispatch info: {:?}", dispatch_info);
            }
            ExtrinsicResult::Failure {
                dispatch_error,
                dispatch_info,
            } => {
                error!(target: LOG_TARGET, "Extrinsic failed with dispatch error: {:?}, dispatch info: {:?}", dispatch_error, dispatch_info);
                return Err(anyhow::anyhow!("Extrinsic failed"));
            }
        }

        info!(target: LOG_TARGET, "Events in extrinsic: {:?}", &extrinsic_in_block.events);

        Ok(())
    }
}
