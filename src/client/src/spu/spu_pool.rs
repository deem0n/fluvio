use std::collections::HashMap;
use std::sync::Arc;

use tracing::debug;
use async_mutex::Mutex;

use dataplane::ReplicaKey;
use dataplane::api::Request;
use dataplane::api::RequestMessage;
use fluvio_types::SpuId;
use fluvio_socket::AllMultiplexerSocket;
use fluvio_socket::AsyncResponse;
use crate::FluvioError;
use crate::client::ClientConfig;
use crate::sync::MetadataStores;
use crate::client::VersionedSerialSocket;
use crate::client::Versions;

const DEFAULT_STREAM_QUEUE_SIZE: usize = 10;

struct SpuSocket {
    config: ClientConfig,
    socket: AllMultiplexerSocket,
    versions: Versions,
}

impl SpuSocket {
    async fn create_serial_socket(&mut self) -> VersionedSerialSocket {
        VersionedSerialSocket::new(
            self.socket.create_serial_socket().await,
            self.config.clone(),
            self.versions.clone(),
        )
    }

    async fn create_stream<R: Request>(
        &mut self,
        request: R,
    ) -> Result<AsyncResponse<R>, FluvioError> {
        let req_msg = RequestMessage::new_request(request);
        self.socket
            .create_stream(req_msg, DEFAULT_STREAM_QUEUE_SIZE)
            .await
            .map_err(|err| err.into())
    }
}

/// connection pool to spu
#[derive(Clone)]
pub struct SpuPool {
    config: ClientConfig,
    metadata: MetadataStores,
    spu_clients: Arc<Mutex<HashMap<SpuId, SpuSocket>>>,
}

impl SpuPool {
    /// create new spu pool from client config template and metadata store
    pub fn new(config: ClientConfig, metadata: MetadataStores) -> Self {
        Self {
            metadata,
            config,
            spu_clients: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// create new spu socket
    async fn connect_to_leader(&self, leader: SpuId) -> Result<SpuSocket, FluvioError> {
        let spu = self.metadata.spus().look_up_by_id(leader).await?;

        debug!("connecting to spu: {}", spu.spec);
        let mut client_config = self.config.clone();
        let spu_addr = spu.spec.public_endpoint.addr();
        debug!("spu addr: {}", spu_addr);
        client_config.set_addr(spu_addr);
        let versioned_socket = client_config.connect().await?;
        let (socket, config, versions) = versioned_socket.split();
        Ok(SpuSocket {
            socket: AllMultiplexerSocket::new(socket),
            config,
            versions,
        })
    }

    // create serial socket connection to replica
    pub async fn create_serial_socket(
        &self,
        replica: &ReplicaKey,
    ) -> Result<VersionedSerialSocket, FluvioError> {
        let partition = self.metadata.partitions().lookup_by_key(replica).await?;

        let leader_id = partition.spec.leader;

        // check if already have existing leader
        let mut client_lock = self.spu_clients.lock().await;

        if let Some(spu_socket) = client_lock.get_mut(&leader_id) {
            return Ok(spu_socket.create_serial_socket().await);
        }

        let mut spu_socket = self.connect_to_leader(leader_id).await?;
        let serial_socket = spu_socket.create_serial_socket().await;
        client_lock.insert(leader_id, spu_socket);

        Ok(serial_socket)
    }

    // create stream to replica
    pub async fn create_stream<R: Request>(
        &self,
        replica: &ReplicaKey,
        request: R,
    ) -> Result<AsyncResponse<R>, FluvioError> {
        let partition = self.metadata.partitions().lookup_by_key(replica).await?;

        let leader_id = partition.spec.leader;

        // check if already have existing leader
        let mut client_lock = self.spu_clients.lock().await;

        if let Some(spu_socket) = client_lock.get_mut(&leader_id) {
            return spu_socket.create_stream(request).await;
        }

        let mut spu_socket = self.connect_to_leader(leader_id).await?;
        let stream = spu_socket.create_stream(request).await?;
        client_lock.insert(leader_id, spu_socket);

        Ok(stream)
    }

    pub fn shutdown(&mut self) {
        self.metadata.shutdown();
    }
}
