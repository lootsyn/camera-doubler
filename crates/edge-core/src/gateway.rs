//! Network-facing, vendor-neutral control gateway backed by Adapter RPCs.

#[cfg(unix)]
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::net::SocketAddr;
#[cfg(unix)]
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
#[cfg(unix)]
use std::time::Duration;

use async_trait::async_trait;
#[cfg(unix)]
use robot_multicam_adapter_client::client::AdapterConnection;
use robot_multicam_protocol::adapter::{CommandEnvelope, CommandFeedback, CommandStatus};
use robot_multicam_protocol::backend::control_gateway_server::{
    ControlGateway as ControlGatewayRpc, ControlGatewayServer,
};
use robot_multicam_protocol::backend::{
    AcquireLeaseRequest, AcquireLeaseResponse, GetManifestRequest, GetManifestResponse,
    ReleaseLeaseRequest, ReleaseLeaseResponse, TimelineCommand, TimelineFeedback,
    TimelineFeedbackStatus,
};
use thiserror::Error;
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};
use uuid::Uuid;

use crate::control::{CommandRequest, ControlError, ControlGateway};

#[derive(Debug, Error)]
pub enum GatewayError {
    #[error("control gateway bind address is invalid")]
    Bind,
    #[error("control gateway server failed: {0}")]
    Server(#[from] tonic::transport::Error),
}

#[derive(Debug, Clone)]
pub struct MutualTls {
    pub certificate_pem: Vec<u8>,
    pub private_key_pem: Vec<u8>,
    pub client_ca_pem: Vec<u8>,
}

#[async_trait]
pub trait CommandRouter: Send + Sync + 'static {
    async fn route(&self, command: CommandEnvelope) -> Result<CommandFeedback, String>;
}

#[derive(Debug, Clone)]
#[cfg(unix)]
pub struct UnixAdapterRouter {
    routes: BTreeMap<String, PathBuf>,
    timeout: Duration,
}

#[cfg(unix)]
impl UnixAdapterRouter {
    pub fn new(routes: BTreeMap<String, PathBuf>, timeout: Duration) -> Result<Self, GatewayError> {
        if timeout.is_zero() || routes.values().any(|path| !path.is_absolute()) {
            return Err(GatewayError::Bind);
        }
        Ok(Self { routes, timeout })
    }
}

#[async_trait]
#[cfg(unix)]
impl CommandRouter for UnixAdapterRouter {
    async fn route(&self, command: CommandEnvelope) -> Result<CommandFeedback, String> {
        let socket = self
            .routes
            .get(&command.device_id)
            .ok_or_else(|| "device has no Adapter route".to_owned())?;
        let mut connection = AdapterConnection::connect(socket, self.timeout)
            .await
            .map_err(|error| error.to_string())?;
        connection
            .execute_command(command)
            .await
            .map_err(|error| error.to_string())
    }
}

#[derive(Debug)]
pub struct ControlGatewayService<R> {
    control: Arc<Mutex<ControlGateway>>,
    router: Arc<R>,
    manifest: Arc<RwLock<Vec<u8>>>,
}

impl<R> ControlGatewayService<R> {
    #[must_use]
    pub fn new(control: ControlGateway, router: Arc<R>, manifest: Arc<RwLock<Vec<u8>>>) -> Self {
        Self {
            control: Arc::new(Mutex::new(control)),
            router,
            manifest,
        }
    }
}

type FeedbackStream =
    Pin<Box<dyn tokio_stream::Stream<Item = Result<TimelineFeedback, Status>> + Send + 'static>>;

#[tonic::async_trait]
impl<R: CommandRouter> ControlGatewayRpc for ControlGatewayService<R> {
    type CommandStreamStream = FeedbackStream;

    async fn acquire_lease(
        &self,
        request: Request<AcquireLeaseRequest>,
    ) -> Result<Response<AcquireLeaseResponse>, Status> {
        let request = request.into_inner();
        let now = now()?;
        let ttl_ns = u64::from(request.ttl_ms)
            .checked_mul(1_000_000)
            .ok_or_else(|| Status::invalid_argument("lease TTL overflow"))?;
        let devices = request.device_ids.into_iter().collect::<BTreeSet<_>>();
        match self
            .control
            .lock()
            .await
            .acquire(&request.client_id, devices, ttl_ns, now)
        {
            Ok(lease) => Ok(Response::new(AcquireLeaseResponse {
                granted: true,
                lease_id: lease.lease_id.as_bytes().to_vec(),
                expires_at_edge_ns: lease.expires_at_edge_ns,
                reason: String::new(),
            })),
            Err(error) => Ok(Response::new(AcquireLeaseResponse {
                reason: error.to_string(),
                ..Default::default()
            })),
        }
    }

    async fn release_lease(
        &self,
        request: Request<ReleaseLeaseRequest>,
    ) -> Result<Response<ReleaseLeaseResponse>, Status> {
        let lease_id = parse_uuid(&request.into_inner().lease_id, "lease_id")?;
        let released = self.control.lock().await.release(lease_id);
        Ok(Response::new(ReleaseLeaseResponse { released }))
    }

    async fn command_stream(
        &self,
        request: Request<Streaming<TimelineCommand>>,
    ) -> Result<Response<Self::CommandStreamStream>, Status> {
        let mut input = request.into_inner();
        let control = Arc::clone(&self.control);
        let router = Arc::clone(&self.router);
        let (sender, receiver) = mpsc::channel(32);
        tokio::spawn(async move {
            loop {
                let command = match input.message().await {
                    Ok(Some(command)) => command,
                    Ok(None) => break,
                    Err(error) => {
                        let _ = sender.send(Err(error)).await;
                        break;
                    }
                };
                let feedback = handle_command(&control, router.as_ref(), command).await;
                if sender.send(feedback).await.is_err() {
                    break;
                }
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(receiver))))
    }

    async fn get_manifest(
        &self,
        _request: Request<GetManifestRequest>,
    ) -> Result<Response<GetManifestResponse>, Status> {
        let manifest = self.manifest.read().await.clone();
        if manifest.is_empty() {
            return Err(Status::failed_precondition("session manifest is not ready"));
        }
        Ok(Response::new(GetManifestResponse {
            serialized_session_manifest: manifest,
        }))
    }
}

async fn handle_command<R: CommandRouter>(
    control: &Mutex<ControlGateway>,
    router: &R,
    command: TimelineCommand,
) -> Result<TimelineFeedback, Status> {
    let command_id = parse_uuid(&command.command_id, "command_id")?;
    let lease_id = parse_uuid(&command.lease_id, "lease_id")?;
    let validated = control
        .lock()
        .await
        .validate_command(
            CommandRequest {
                command_id,
                lease_id,
                device_id: command.device_id.clone(),
                command_mode: command.command_mode.clone(),
                action_schema_id: command.action_schema_id,
                values: command.values,
            },
            now()?,
        )
        .map_err(control_status)?;
    let envelope = CommandEnvelope {
        command_id: validated.command_id.as_bytes().to_vec(),
        lease_id: validated.lease_id.as_bytes().to_vec(),
        device_id: validated.device_id.clone(),
        command_mode: validated.command_mode,
        action_schema_id: validated.action_schema_id,
        values: validated.values,
        client_time_ns: command.client_time_ns,
    };
    let feedback = router.route(envelope).await.map_err(Status::unavailable)?;
    Ok(TimelineFeedback {
        command_id: feedback.command_id,
        device_id: feedback.device_id,
        status: map_status(feedback.status),
        observed_edge_ns: now()?,
        effective_values: feedback.effective_values,
        reason: feedback.reason,
    })
}

pub async fn serve_control<R: CommandRouter>(
    bind: &str,
    service: ControlGatewayService<R>,
    tls: MutualTls,
) -> Result<(), GatewayError> {
    let address: SocketAddr = bind.parse().map_err(|_| GatewayError::Bind)?;
    if tls.certificate_pem.is_empty()
        || tls.private_key_pem.is_empty()
        || tls.client_ca_pem.is_empty()
    {
        return Err(GatewayError::Bind);
    }
    let identity = tonic::transport::Identity::from_pem(tls.certificate_pem, tls.private_key_pem);
    let client_ca = tonic::transport::Certificate::from_pem(tls.client_ca_pem);
    tonic::transport::Server::builder()
        .tls_config(
            tonic::transport::ServerTlsConfig::new()
                .identity(identity)
                .client_ca_root(client_ca),
        )?
        .add_service(ControlGatewayServer::new(service))
        .serve(address)
        .await?;
    Ok(())
}

fn map_status(status: i32) -> i32 {
    match CommandStatus::try_from(status) {
        Ok(CommandStatus::Received | CommandStatus::Validated) => {
            TimelineFeedbackStatus::Routed as i32
        }
        Ok(CommandStatus::Accepted) => TimelineFeedbackStatus::Accepted as i32,
        Ok(CommandStatus::Active) => TimelineFeedbackStatus::Active as i32,
        Ok(CommandStatus::Completed) => TimelineFeedbackStatus::Completed as i32,
        Ok(CommandStatus::Rejected) => TimelineFeedbackStatus::Rejected as i32,
        Ok(CommandStatus::Failed | CommandStatus::Unspecified) | Err(_) => {
            TimelineFeedbackStatus::Failed as i32
        }
    }
}

fn parse_uuid(bytes: &[u8], field: &'static str) -> Result<Uuid, Status> {
    Uuid::from_slice(bytes)
        .map_err(|_| Status::invalid_argument(format!("{field} must be 16 bytes")))
}

fn now() -> Result<u64, Status> {
    robot_multicam_timebase::monotonic_now_ns().map_err(|error| Status::internal(error.to_string()))
}

fn control_status(error: ControlError) -> Status {
    Status::failed_precondition(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use async_trait::async_trait;
    use robot_multicam_protocol::adapter::{CommandEnvelope, CommandFeedback, CommandStatus};
    use robot_multicam_protocol::backend::{TimelineCommand, TimelineFeedbackStatus};
    use tokio::sync::Mutex;
    use uuid::Uuid;

    use super::{handle_command, CommandRouter};
    use crate::control::{ControlGateway, DeviceCommandPolicy};

    struct MockRouter;

    #[async_trait]
    impl CommandRouter for MockRouter {
        async fn route(&self, command: CommandEnvelope) -> Result<CommandFeedback, String> {
            Ok(CommandFeedback {
                command_id: command.command_id,
                device_id: command.device_id,
                status: CommandStatus::Completed as i32,
                effective_values: command.values,
                ..Default::default()
            })
        }
    }

    #[tokio::test]
    async fn validated_command_is_routed_and_effective_values_returned() {
        let mut gateway = ControlGateway::new(
            vec![DeviceCommandPolicy {
                device_id: "body".to_owned(),
                action_schema_id: 7,
                vector_length: 2,
                command_modes: BTreeSet::from(["position".to_owned()]),
                minimum: vec![-1.0; 2],
                maximum: vec![1.0; 2],
            }],
            10,
            u64::MAX / 2,
            8,
        )
        .expect("gateway");
        let lease = gateway
            .acquire("test", BTreeSet::from(["body".to_owned()]), u64::MAX / 4, 0)
            .expect("lease");
        let command_id = Uuid::new_v4();
        let feedback = handle_command(
            &Mutex::new(gateway),
            &MockRouter,
            TimelineCommand {
                command_id: command_id.as_bytes().to_vec(),
                lease_id: lease.lease_id.as_bytes().to_vec(),
                device_id: "body".to_owned(),
                command_mode: "position".to_owned(),
                action_schema_id: 7,
                values: vec![0.25, -0.25],
                ..Default::default()
            },
        )
        .await
        .expect("feedback");
        assert_eq!(feedback.status, TimelineFeedbackStatus::Completed as i32);
        assert_eq!(feedback.effective_values, vec![0.25, -0.25]);
    }
}
