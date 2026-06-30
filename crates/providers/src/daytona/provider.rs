//! SandboxProvider backed by Daytona (https://daytona.io), the first cloud
//! backend, so sandboxes survive the laptop being powered off (PLAN.md M7).
//!
//! Status: implemented against daytona-client 0.5 but NOT yet validated against
//! the live service (needs a DAYTONA_API_KEY). Mappings worth revalidating when
//! a key is available are marked NOTE.
//!
//! Caveat: daytona-client 0.5 calls Daytona's older `/toolbox/{id}/toolbox/...`
//! exec and file endpoints, which the current API marks deprecated (the
//! supported routes are `/process/execute`, `/files/upload`, `/files/download`).
//! They should still work for a first run; if the live test fails here, the
//! follow-up is to hand-roll these calls with reqwest against the current API
//! rather than the unofficial crate. The workspace mount is set by the CLI to a
//! path the `daytona` user can write (/home/daytona/workspace).
//!
//! Mapping onto Daytona:
//! - SandboxSpec.image  -> Daytona snapshot name (Daytona boots from snapshots,
//!   not raw Docker images; build a snapshot from images/base/Dockerfile).
//! - suspend/resume     -> stop/start.
//! - snapshot           -> create_backup.
//! - exec               -> ProcessExecutor (returns combined output; no stderr).
//! - attach_pty         -> not yet; interactive cloud terminals are future work
//!   (Daytona sessions or preview-url/SSH).

use std::collections::HashMap;

use anyhow::anyhow;
use async_trait::async_trait;
use daytona_client::{
    CreateSandboxParams, DaytonaClient, DaytonaConfig, ExecuteRequest, Sandbox as DaytonaSandbox,
    SandboxState,
};
use uuid::Uuid;

use shepherd_core::errors::{Error, Result};
use shepherd_core::ids::SandboxId;
use shepherd_core::sandbox::{
    ConnectionInfo, ExecOptions, ExecResult, PtyOptions, PtySession, Sandbox, SandboxProvider,
    SandboxSpec, SandboxStatus,
};

const MANAGED_LABEL: &str = "shepherd.managed";
const READY_TIMEOUT_SECS: u64 = 180;

// Cost guardrails so a forgotten box cannot quietly run up the bill. Suspend
// (stop) frees CPU and RAM after a short idle; archive (cold storage, no quota
// cost) after a day stopped. Neither deletes anything: resume/restore from
// anywhere, no laptop required. Auto-delete is intentionally left disabled.
const AUTO_STOP_MINUTES: u32 = 20;
const AUTO_ARCHIVE_MINUTES: u32 = 1440; // 1 day

pub struct DaytonaProvider {
    client: DaytonaClient,
    // Kept for the few calls the crate does not cover (ssh-access), which we
    // hand-roll against the REST API with the same auth.
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    organization_id: Option<String>,
}

impl DaytonaProvider {
    /// Build from DAYTONA_API_KEY (and optional DAYTONA_BASE_URL) in the env.
    pub fn from_env() -> Result<Self> {
        Self::from_config(DaytonaConfig::from_env().map_err(dy)?)
    }

    /// Build from an explicit API key.
    pub fn with_api_key(api_key: impl Into<String>) -> Result<Self> {
        Self::from_config(DaytonaConfig::new(api_key))
    }

    fn from_config(config: DaytonaConfig) -> Result<Self> {
        let base_url = config.base_url.clone();
        let api_key = config.api_key.clone();
        let organization_id = config.organization_id.clone();
        let client = DaytonaClient::new(config).map_err(dy)?;
        Ok(Self {
            client,
            http: reqwest::Client::new(),
            base_url,
            api_key,
            organization_id,
        })
    }

    /// Mint a short-lived SSH token and return an `ssh` destination for it.
    /// Hand-rolled because daytona-client 0.5 does not expose ssh-access.
    async fn create_ssh_target(&self, id: &SandboxId) -> Result<String> {
        let url = format!("{}/sandbox/{}/ssh-access?expiresInMinutes=120", self.base_url, id);
        let mut req = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key));
        if let Some(org) = &self.organization_id {
            req = req.header("X-Daytona-Organization-ID", org);
        }
        let resp = req.send().await.map_err(http_err)?.error_for_status().map_err(http_err)?;

        #[derive(serde::Deserialize)]
        struct SshAccessDto {
            token: String,
        }
        let dto: SshAccessDto = resp.json().await.map_err(http_err)?;
        Ok(format!("{}@ssh.app.daytona.io", dto.token))
    }
}

#[async_trait]
impl SandboxProvider for DaytonaProvider {
    fn id(&self) -> String {
        "daytona".to_string()
    }

    async fn create(&self, spec: SandboxSpec) -> Result<Sandbox> {
        let mut labels = spec.labels.clone();
        labels.insert(MANAGED_LABEL.to_string(), "true".to_string());

        let params = CreateSandboxParams {
            // Empty image means use the Daytona default snapshot.
            snapshot: (!spec.image.is_empty()).then(|| spec.image.clone()),
            env: (!spec.env.is_empty()).then(|| spec.env.clone()),
            labels: Some(labels),
            cpu: spec.resources.cpus.map(|c| c.ceil() as u32),
            // NOTE: Daytona sizes memory/disk in GB; our spec is in MB.
            memory: spec.resources.memory_mb.map(|m| (m as f64 / 1024.0).ceil() as u32),
            disk: spec.resources.disk_mb.map(|d| (d as f64 / 1024.0).ceil() as u32),
            auto_stop_interval: Some(AUTO_STOP_MINUTES),
            auto_archive_interval: Some(AUTO_ARCHIVE_MINUTES),
            ..Default::default()
        };

        let created = self.client.sandboxes().create(params).await.map_err(dy)?;
        // Wait until it is actually runnable before anyone execs into it.
        let ready = self
            .client
            .sandboxes()
            .wait_for_state(&created.id, SandboxState::Started, READY_TIMEOUT_SECS)
            .await
            .map_err(dy)?;
        Ok(self.to_sandbox(&ready))
    }

    async fn get(&self, id: &SandboxId) -> Result<Option<Sandbox>> {
        let uuid = parse_id(id)?;
        match self.client.sandboxes().get(&uuid).await {
            Ok(sb) => Ok(Some(self.to_sandbox(&sb))),
            Err(e) if is_not_found(&e) => Ok(None),
            Err(e) => Err(dy(e)),
        }
    }

    async fn list(&self, labels: &HashMap<String, String>) -> Result<Vec<Sandbox>> {
        let all = self.client.sandboxes().list().await.map_err(dy)?;
        Ok(all
            .into_iter()
            .filter(|sb| sb.labels.get(MANAGED_LABEL).map(String::as_str) == Some("true"))
            .filter(|sb| labels.iter().all(|(k, v)| sb.labels.get(k) == Some(v)))
            .map(|sb| self.to_sandbox(&sb))
            .collect())
    }

    async fn exec(&self, id: &SandboxId, command: &[String], opts: ExecOptions) -> Result<ExecResult> {
        let uuid = parse_id(id)?;
        let request = ExecuteRequest {
            command: shell_join(command),
            cwd: opts.cwd,
            timeout: None,
        };
        let resp = self
            .client
            .process()
            .execute_with_options(&uuid, request)
            .await
            .map_err(dy)?;
        // Daytona returns combined output in `result`, with no separate stderr.
        Ok(ExecResult {
            exit_code: resp.exit_code as i64,
            stdout: resp.result,
            stderr: String::new(),
        })
    }

    async fn attach_pty(&self, _id: &SandboxId, _command: &[String], _opts: PtyOptions) -> Result<PtySession> {
        // No in-process PTY stream over the REST crate. Interactive attach goes
        // through connection_info (ssh / web terminal) instead.
        Err(self.not_supported("attach_pty"))
    }

    async fn connection_info(&self, id: &SandboxId) -> Result<ConnectionInfo> {
        Ok(ConnectionInfo {
            // Deterministic per-sandbox browser terminal (open it from a phone).
            web_terminal_url: Some(format!("https://22222-{id}.proxy.daytona.work")),
            // Best-effort: minting a token can fail (e.g. box stopped); the web
            // terminal still works, so do not hard-fail attach on this.
            ssh_target: self.create_ssh_target(id).await.ok(),
        })
    }

    async fn put_file(&self, id: &SandboxId, path: &str, content: &[u8], _mode: u32) -> Result<()> {
        let uuid = parse_id(id)?;
        self.client.files().upload(&uuid, path, content).await.map_err(dy)
    }

    async fn get_file(&self, id: &SandboxId, path: &str) -> Result<Vec<u8>> {
        let uuid = parse_id(id)?;
        self.client.files().download(&uuid, path).await.map_err(dy)
    }

    async fn snapshot(&self, id: &SandboxId) -> Result<String> {
        let uuid = parse_id(id)?;
        let sb = self.client.sandboxes().create_backup(&uuid, None).await.map_err(dy)?;
        Ok(sb.backup_state)
    }

    async fn suspend(&self, id: &SandboxId) -> Result<()> {
        let uuid = parse_id(id)?;
        self.client.sandboxes().stop(&uuid).await.map_err(dy)
    }

    async fn resume(&self, id: &SandboxId) -> Result<()> {
        let uuid = parse_id(id)?;
        self.client.sandboxes().start(&uuid).await.map_err(dy)
    }

    async fn destroy(&self, id: &SandboxId) -> Result<()> {
        let uuid = parse_id(id)?;
        self.client.sandboxes().delete_with_force(&uuid, true).await.map_err(dy)
    }
}

impl DaytonaProvider {
    fn to_sandbox(&self, sb: &DaytonaSandbox) -> Sandbox {
        Sandbox {
            id: SandboxId::from(sb.id.to_string()),
            provider_id: self.id(),
            status: map_state(&sb.state),
            image: sb.snapshot.clone(),
            created_at: sb.created_at.to_rfc3339(),
            labels: sb.labels.clone(),
        }
    }
}

/// Map a Daytona error into our error type.
fn dy(e: daytona_client::DaytonaError) -> Error {
    Error::Other(anyhow!(e.to_string()))
}

fn http_err(e: reqwest::Error) -> Error {
    Error::Other(anyhow!("daytona ssh-access request failed: {e}"))
}

fn is_not_found(e: &daytona_client::DaytonaError) -> bool {
    let s = e.to_string().to_lowercase();
    s.contains("404") || s.contains("not found")
}

fn parse_id(id: &SandboxId) -> Result<Uuid> {
    Uuid::parse_str(id.as_str()).map_err(|_| Error::SandboxNotFound(id.to_string()))
}

fn map_state(state: &SandboxState) -> SandboxStatus {
    use SandboxState::*;
    match state {
        Started => SandboxStatus::Running,
        Starting | Creating | Restoring | PendingBuild | BuildingSnapshot | PullingSnapshot => {
            SandboxStatus::Creating
        }
        Stopped | Stopping => SandboxStatus::Suspended,
        Archived | Archiving | Destroyed | Destroying => SandboxStatus::Stopped,
        Error | BuildFailed | Unknown => SandboxStatus::Error,
    }
}

/// Join an argv into a single shell command, quoting args that need it. Daytona
/// executes a command string, not an argv vector.
fn shell_join(args: &[String]) -> String {
    args.iter().map(|a| shell_quote(a)).collect::<Vec<_>>().join(" ")
}

fn shell_quote(arg: &str) -> String {
    let safe = !arg.is_empty()
        && arg
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'/' | b':' | b'=' | b','));
    if safe {
        arg.to_string()
    } else {
        // Single-quote and escape embedded single quotes.
        format!("'{}'", arg.replace('\'', r#"'\''"#))
    }
}
