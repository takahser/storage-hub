// This file is part of Substrate.

// Copyright (C) Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.

//! Helper for incoming provider client requests.
//!
//! Handle (i.e. answer) incoming provider client requests from a remote peer received via
//! `crate::request_responses::RequestResponsesBehaviour` with
//! [`LightClientRequestHandler`](handler::LightClientRequestHandler).

use std::sync::Arc;

use anyhow::Result;
use futures::prelude::*;
use futures::stream::select;
use libp2p_identity::PeerId;
use prost::Message;
use sc_network::{
    request_responses::{IncomingRequest, OutgoingResponse},
    ReputationChange,
};
use sc_tracing::tracing::{debug, info, trace, warn};
use storage_hub_infra::actor::{Actor, ActorEventLoop};

use crate::{
    service::ParachainNetworkService, services::file_transfer::events::RemoteUploadRequest,
};

use super::{events::FileTransferServiceEventBusProvider, schema};

const LOG_TARGET: &str = "file-transfer-service";

#[derive(Debug)]
pub enum FileTransferServiceCommand {}

pub struct FileTransferService {
    request_receiver: async_channel::Receiver<IncomingRequest>,
    _network: Arc<ParachainNetworkService>,
    event_bus_provider: FileTransferServiceEventBusProvider,
}

impl Actor for FileTransferService {
    type Message = FileTransferServiceCommand;
    type EventLoop = FileTransferServiceEventLoop;
    type EventBusProvider = FileTransferServiceEventBusProvider;

    fn handle_message(
        &mut self,
        _message: Self::Message,
    ) -> impl std::future::Future<Output = ()> + Send {
        async {}
    }

    fn get_event_bus_provider(&self) -> &Self::EventBusProvider {
        &self.event_bus_provider
    }
}

/// Event loop for the FileTransferService actor.
pub struct FileTransferServiceEventLoop {
    receiver: sc_utils::mpsc::TracingUnboundedReceiver<FileTransferServiceCommand>,
    actor: FileTransferService,
}

enum MergedEventLoopMessage {
    Command(FileTransferServiceCommand),
    Request(IncomingRequest),
}

/// Since this actor is a network service, it needs to handle both incoming network events and
/// messages from other actors, hence the need for a custom `ActorEventLoop`.
impl ActorEventLoop<FileTransferService> for FileTransferServiceEventLoop {
    fn new(
        actor: FileTransferService,
        receiver: sc_utils::mpsc::TracingUnboundedReceiver<FileTransferServiceCommand>,
    ) -> Self {
        Self { actor, receiver }
    }

    async fn run(mut self) {
        info!(target: LOG_TARGET, "FileTransferService starting up!");

        let mut merged_stream = select(
            self.receiver.map(MergedEventLoopMessage::Command),
            self.actor
                .request_receiver
                .clone()
                .map(MergedEventLoopMessage::Request),
        );

        loop {
            match merged_stream.next().await {
                Some(MergedEventLoopMessage::Command(command)) => {
                    self.actor.handle_message(command).await;
                }
                Some(MergedEventLoopMessage::Request(request)) => {
                    let IncomingRequest {
                        peer,
                        payload,
                        pending_response,
                    } = request;

                    match self.actor.handle_request(peer, payload) {
                        Ok(response_data) => {
                            let response = OutgoingResponse {
                                result: Ok(response_data),
                                reputation_changes: Vec::new(),
                                sent_feedback: None,
                            };

                            match pending_response.send(response) {
                                Ok(()) => trace!(
                                    target: LOG_TARGET,
                                    "Handled provider client request from {}.",
                                    peer,
                                ),
                                Err(_) => debug!(
                                    target: LOG_TARGET,
                                    "Failed to handle provider request from {}: {}",
                                    peer,
                                    HandleRequestError::SendResponse,
                                ),
                            };
                        }
                        Err(e) => {
                            debug!(
                                target: LOG_TARGET,
                                "Failed to handle provider client request from {}: {}", peer, e,
                            );

                            let reputation_changes = match e {
                                HandleRequestError::BadRequest(_) => {
                                    vec![ReputationChange::new(-(1 << 12), "bad request")]
                                }
                                _ => Vec::new(),
                            };

                            let response = OutgoingResponse {
                                result: Err(()),
                                reputation_changes,
                                sent_feedback: None,
                            };

                            if pending_response.send(response).is_err() {
                                debug!(
                                    target: LOG_TARGET,
                                    "Failed to handle provider client request from {}: {}",
                                    peer,
                                    HandleRequestError::SendResponse,
                                );
                            };
                        }
                    }
                }
                None => {
                    warn!(target: LOG_TARGET, "FileTransferService event loop terminated.");
                    break;
                }
            }
        }
    }
}

impl FileTransferService {
    /// Create a new [`FileTransferService`].
    pub fn new(
        request_receiver: async_channel::Receiver<IncomingRequest>,
        network: Arc<ParachainNetworkService>,
    ) -> Self {
        Self {
            request_receiver,
            _network: network,
            event_bus_provider: FileTransferServiceEventBusProvider::new(),
        }
    }

    fn handle_request(
        &mut self,
        peer: PeerId,
        payload: Vec<u8>,
    ) -> Result<Vec<u8>, HandleRequestError> {
        let request = schema::v1::provider::Request::decode(&payload[..])?;

        let response = match &request.request {
            Some(schema::v1::provider::request::Request::RemoteUploadDataRequest(r)) => {
                self.on_remote_upload_data_request(&peer, r)?
            }
            Some(schema::v1::provider::request::Request::RemoteDownloadDataRequest(r)) => {
                self.on_remote_download_data_request(&peer, r)?
            }
            None => {
                return Err(HandleRequestError::BadRequest(
                    "Remote request without request data.",
                ))
            }
        };

        let mut data = Vec::new();
        response.encode(&mut data)?;

        Ok(data)
    }

    fn on_remote_upload_data_request(
        &mut self,
        peer: &PeerId,
        request: &schema::v1::provider::RemoteUploadDataRequest,
    ) -> Result<schema::v1::provider::Response, HandleRequestError> {
        trace!(
            "Remote upload data request from {} (FileKey: {:?})",
            peer,
            request.file_key
        );

        self.emit(RemoteUploadRequest {
            location: "".to_string(),
        });

        // TODO actually save data.
        let response = schema::v1::provider::RemoteUploadDataResponse { success: true };

        Ok(schema::v1::provider::Response {
            response: Some(
                schema::v1::provider::response::Response::RemoteUploadDataResponse(response),
            ),
        })
    }

    fn on_remote_download_data_request(
        &mut self,
        peer: &PeerId,
        request: &schema::v1::provider::RemoteDownloadDataRequest,
    ) -> Result<schema::v1::provider::Response, HandleRequestError> {
        trace!(
            "Remote download data request from {} (FileKey: {:?}, ChunkId: {}).",
            peer,
            request.file_key,
            request.file_chunk_id,
        );

        // TODO actually read data.
        let response = schema::v1::provider::RemoteDownloadDataResponse {
            file_chunk_with_proof: vec![],
        };

        Ok(schema::v1::provider::Response {
            response: Some(
                schema::v1::provider::response::Response::RemoteDownloadDataResponse(response),
            ),
        })
    }
}

#[derive(Debug, thiserror::Error)]
enum HandleRequestError {
    #[error("Failed to decode request: {0}.")]
    DecodeProto(#[from] prost::DecodeError),
    #[error("Failed to encode response: {0}.")]
    EncodeProto(#[from] prost::EncodeError),
    #[error("Failed to send response.")]
    SendResponse,
    /// A bad request has been received.
    #[error("bad request: {0}")]
    BadRequest(&'static str),
    /// Encoding or decoding of some data failed.
    #[error("codec error: {0}")]
    Codec(#[from] codec::Error),
}
