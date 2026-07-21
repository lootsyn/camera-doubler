//! Shared configuration, error, telemetry, and health primitives.

use std::collections::BTreeMap;
use std::env;
use std::str::FromStr;
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("missing required environment variable {0}")]
    Missing(&'static str),
    #[error("invalid value for {key}: {value}")]
    Invalid { key: &'static str, value: String },
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

pub fn required_env(key: &'static str) -> Result<String, ConfigError> {
    env::var(key).map_err(|_| ConfigError::Missing(key))
}

pub fn env_or<T>(key: &'static str, default: T) -> Result<T, ConfigError>
where
    T: FromStr,
{
    match env::var(key) {
        Ok(value) => value
            .parse()
            .map_err(|_| ConfigError::Invalid { key, value }),
        Err(env::VarError::NotPresent) => Ok(default),
        Err(env::VarError::NotUnicode(value)) => Err(ConfigError::Invalid {
            key,
            value: value.to_string_lossy().into_owned(),
        }),
    }
}

pub fn init_tracing(service: &str) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_thread_ids(true)
        .json()
        .try_init();
    tracing::info!(service, "telemetry initialized");
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HealthSnapshot {
    pub live: bool,
    pub ready: bool,
    pub status: String,
    pub checks: BTreeMap<String, CheckStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Pass,
    Degraded(String),
    Fail(String),
}

impl HealthSnapshot {
    #[must_use]
    pub fn from_checks(checks: BTreeMap<String, CheckStatus>) -> Self {
        let ready = checks
            .values()
            .all(|status| matches!(status, CheckStatus::Pass | CheckStatus::Degraded(_)));
        Self {
            live: true,
            ready,
            status: if ready { "ready" } else { "not_ready" }.to_owned(),
            checks,
        }
    }
}

#[derive(Debug, Default)]
pub struct Observability {
    checks: Mutex<BTreeMap<String, CheckStatus>>,
    counters: Mutex<BTreeMap<String, u64>>,
}

impl Observability {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_check(&self, name: &str, status: CheckStatus) -> Result<(), ConfigError> {
        validate_metric_name(name)?;
        self.checks
            .lock()
            .map_err(|_| ConfigError::Invalid {
                key: "OBSERVABILITY_LOCK",
                value: "poisoned".to_owned(),
            })?
            .insert(name.to_owned(), status);
        Ok(())
    }

    pub fn increment(&self, name: &str, amount: u64) -> Result<(), ConfigError> {
        validate_metric_name(name)?;
        let mut counters = self.counters.lock().map_err(|_| ConfigError::Invalid {
            key: "OBSERVABILITY_LOCK",
            value: "poisoned".to_owned(),
        })?;
        let counter = counters.entry(name.to_owned()).or_default();
        *counter = counter.saturating_add(amount);
        Ok(())
    }

    pub fn snapshot(&self) -> Result<HealthSnapshot, ConfigError> {
        Ok(HealthSnapshot::from_checks(
            self.checks
                .lock()
                .map_err(|_| ConfigError::Invalid {
                    key: "OBSERVABILITY_LOCK",
                    value: "poisoned".to_owned(),
                })?
                .clone(),
        ))
    }

    pub fn prometheus(&self) -> Result<String, ConfigError> {
        let counters = self.counters.lock().map_err(|_| ConfigError::Invalid {
            key: "OBSERVABILITY_LOCK",
            value: "poisoned".to_owned(),
        })?;
        let mut output = String::new();
        for (name, value) in counters.iter() {
            output.push_str("# TYPE ");
            output.push_str(name);
            output.push_str(" counter\n");
            output.push_str(name);
            output.push(' ');
            output.push_str(&value.to_string());
            output.push('\n');
        }
        Ok(output)
    }
}

fn validate_metric_name(name: &str) -> Result<(), ConfigError> {
    if name.is_empty()
        || name.len() > 128
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b':'))
    {
        return Err(ConfigError::Invalid {
            key: "METRIC_NAME",
            value: name.to_owned(),
        });
    }
    Ok(())
}

pub async fn serve_observability(
    bind: &str,
    state: std::sync::Arc<Observability>,
) -> Result<(), std::io::Error> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind(bind).await?;
    loop {
        let (mut socket, _) = listener.accept().await?;
        let state = std::sync::Arc::clone(&state);
        tokio::spawn(async move {
            let mut request = [0_u8; 1024];
            let read = tokio::time::timeout(Duration::from_secs(2), socket.read(&mut request))
                .await
                .ok()
                .and_then(Result::ok)
                .unwrap_or(0);
            let path = std::str::from_utf8(&request[..read])
                .ok()
                .and_then(|text| text.split_whitespace().nth(1))
                .unwrap_or("");
            let (status, content_type, body) = match path {
                "/healthz" | "/readyz" => match state.snapshot() {
                    Ok(snapshot) => {
                        let status = if path == "/readyz" && !snapshot.ready {
                            "503 Service Unavailable"
                        } else {
                            "200 OK"
                        };
                        (
                            status,
                            "application/json",
                            serde_json::to_string(&snapshot).unwrap_or_else(|_| "{}".to_owned()),
                        )
                    }
                    Err(error) => ("503 Service Unavailable", "text/plain", error.to_string()),
                },
                "/metrics" => match state.prometheus() {
                    Ok(metrics) => ("200 OK", "text/plain; version=0.0.4", metrics),
                    Err(error) => ("503 Service Unavailable", "text/plain", error.to_string()),
                },
                _ => ("404 Not Found", "text/plain", "not found\n".to_owned()),
            };
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{CheckStatus, HealthSnapshot, Observability};
    use std::collections::BTreeMap;

    #[test]
    fn failed_check_fails_readiness_without_liveness() {
        let checks = BTreeMap::from([("camera".to_owned(), CheckStatus::Fail("gone".to_owned()))]);
        let health = HealthSnapshot::from_checks(checks);
        assert!(health.live);
        assert!(!health.ready);
    }

    #[test]
    fn prometheus_metrics_are_named_and_saturating() {
        let telemetry = Observability::new();
        telemetry
            .increment("camera_frames_total", 2)
            .expect("counter");
        telemetry
            .increment("camera_frames_total", 3)
            .expect("counter");
        assert!(telemetry
            .prometheus()
            .expect("metrics")
            .contains("camera_frames_total 5"));
        assert!(telemetry.increment("bad-name", 1).is_err());
    }
}
