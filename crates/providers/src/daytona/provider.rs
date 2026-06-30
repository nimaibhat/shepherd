//! SandboxProvider backed by Daytona (https://daytona.io), the first cloud
//! backend, so sandboxes survive the laptop being powered off (PLAN.md M7).
//!
//! Hand-rolled against the Daytona REST API with reqwest and lenient parsing.
//! (The community daytona-client crate was dropped: it deserializes responses
//! into strict structs that drift from the live API, e.g. a now-absent `class`
//! field, breaking the very first call.)
//!
//! Mapping onto Daytona:
//! - SandboxSpec.image  -> Daytona snapshot name (Daytona boots from snapshots,
//!   not raw Docker images; empty means the account default snapshot).
//! - suspend/resume     -> stop/start.
//! - snapshot           -> backup.
//! - exec               -> toolbox process/execute (combined output, no stderr).
//! - put/get file       -> toolbox files bulk-upload / download.
//! - attach             -> connection_info (web terminal + ssh), see attach.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::anyhow;
use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use shepherd_core::errors::{Error, Result};
use shepherd_core::ids::SandboxId;
use shepherd_core::sandbox::{
    ConnectionInfo, ExecOptions, ExecResult, PtyOptions, PtySession, Sandbox, SandboxProvider,
    SandboxSpec, SandboxStatus,
};

const MANAGED_LABEL: &str = "shepherd.managed";
const READY_TIMEOUT: Duration = Duration::from_secs(180);
const DEFAULT_BASE_URL: &str = "https://app.daytona.io/api";

// Cost guardrails so a forgotten box cannot quietly run up the bill. Suspend
// (stop) frees CPU and RAM after a short idle; archive (cold storage, no quota
// cost) after a day stopped. Neither deletes anything; resume/restore from
// anywhere, no laptop required.
const AUTO_STOP_MINUTES: u32 = 20;
const AUTO_ARCHIVE_MINUTES: u32 = 1440; // 1 day

pub struct DaytonaProvider {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    org: Option<String>,
}

impl DaytonaProvider {
    /// Build from DAYTONA_API_KEY (and optional DAYTONA_BASE_URL,
    /// DAYTONA_ORGANIZATION_ID) in the environment.
    pub fn from_env() -> Result<Self> {
        let api_key = std::env::var("DAYTONA_API_KEY")
            .map_err(|_| Error::Other(anyhow!("DAYTONA_API_KEY environment variable not set")))?;
        Ok(Self::build(api_key, std::env::var("DAYTONA_BASE_URL").ok(), std::env::var("DAYTONA_ORGANIZATION_ID").ok()))
    }

    /// Build from an explicit API key.
    pub fn with_api_key(api_key: impl Into<String>) -> Result<Self> {
        Ok(Self::build(api_key.into(), None, None))
    }

    fn build(api_key: String, base_url: Option<String>, org: Option<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
            api_key,
            org,
        }
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let mut rb = self
            .http
            .request(method, format!("{}{}", self.base_url, path))
            .header("Authorization", format!("Bearer {}", self.api_key));
        if let Some(org) = &self.org {
            rb = rb.header("X-Daytona-Organization-ID", org);
        }
        rb
    }

    async fn get_raw(&self, id: &SandboxId) -> Result<Option<DaytonaSandbox>> {
        let resp = self
            .req(reqwest::Method::GET, &format!("/sandbox/{id}"))
            .send()
            .await
            .map_err(http_err)?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        Ok(Some(parse_checked(resp).await?))
    }

    async fn wait_started(&self, id: &SandboxId) -> Result<DaytonaSandbox> {
        let start = std::time::Instant::now();
        loop {
            let sb = self
                .get_raw(id)
                .await?
                .ok_or_else(|| Error::SandboxNotFound(id.to_string()))?;
            match map_state(sb.state.as_deref().unwrap_or("")) {
                SandboxStatus::Running => return Ok(sb),
                SandboxStatus::Error => {
                    return Err(Error::Other(anyhow!("sandbox {id} entered error state")))
                }
                _ => {}
            }
            if start.elapsed() > READY_TIMEOUT {
                return Err(Error::Other(anyhow!("timed out waiting for sandbox {id} to start")));
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    async fn create_ssh_target(&self, id: &SandboxId) -> Result<String> {
        let resp = self
            .req(reqwest::Method::POST, &format!("/sandbox/{id}/ssh-access?expiresInMinutes=120"))
            .send()
            .await
            .map_err(http_err)?;
        #[derive(Deserialize)]
        struct SshAccessDto {
            token: String,
        }
        let dto: SshAccessDto = parse_checked(resp).await?;
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

        let mut body = Map::new();
        if !spec.image.is_empty() {
            body.insert("snapshot".into(), json!(spec.image));
        }
        if !spec.env.is_empty() {
            body.insert("env".into(), json!(spec.env));
        }
        body.insert("labels".into(), json!(labels));
        if let Some(c) = spec.resources.cpus {
            body.insert("cpu".into(), json!(c.ceil() as u32));
        }
        if let Some(m) = spec.resources.memory_mb {
            body.insert("memory".into(), json!((m as f64 / 1024.0).ceil() as u32));
        }
        if let Some(d) = spec.resources.disk_mb {
            body.insert("disk".into(), json!((d as f64 / 1024.0).ceil() as u32));
        }
        body.insert("autoStopInterval".into(), json!(AUTO_STOP_MINUTES));
        body.insert("autoArchiveInterval".into(), json!(AUTO_ARCHIVE_MINUTES));

        let resp = self
            .req(reqwest::Method::POST, "/sandbox")
            .json(&Value::Object(body))
            .send()
            .await
            .map_err(http_err)?;
        let created: DaytonaSandbox = parse_checked(resp).await?;
        let id = SandboxId::from(created.id);
        let ready = self.wait_started(&id).await?;
        Ok(ready.into_sandbox(id, self.id()))
    }

    async fn get(&self, id: &SandboxId) -> Result<Option<Sandbox>> {
        Ok(self.get_raw(id).await?.map(|sb| sb.into_sandbox(id.clone(), self.id())))
    }

    async fn list(&self, labels: &HashMap<String, String>) -> Result<Vec<Sandbox>> {
        let resp = self
            .req(reqwest::Method::GET, "/sandbox?limit=100")
            .send()
            .await
            .map_err(http_err)?;
        let value: Value = parse_checked(resp).await?;
        // The endpoint may return a bare array or a paginated { items: [...] }.
        let items = value
            .as_array()
            .cloned()
            .or_else(|| value.get("items").and_then(|i| i.as_array()).cloned())
            .unwrap_or_default();

        let mut out = Vec::new();
        for v in items {
            let Ok(sb) = serde_json::from_value::<DaytonaSandbox>(v) else { continue };
            if sb.labels.get(MANAGED_LABEL).map(String::as_str) != Some("true") {
                continue;
            }
            if !labels.iter().all(|(k, val)| sb.labels.get(k) == Some(val)) {
                continue;
            }
            let id = SandboxId::from(sb.id.clone());
            out.push(sb.into_sandbox(id, self.id()));
        }
        Ok(out)
    }

    async fn exec(&self, id: &SandboxId, command: &[String], opts: ExecOptions) -> Result<ExecResult> {
        let mut body = Map::new();
        body.insert("command".into(), json!(shell_join(command)));
        if let Some(cwd) = &opts.cwd {
            body.insert("cwd".into(), json!(cwd));
        }
        let resp = self
            .req(reqwest::Method::POST, &format!("/toolbox/{id}/toolbox/process/execute"))
            .json(&Value::Object(body))
            .send()
            .await
            .map_err(http_err)?;

        #[derive(Deserialize)]
        struct ExecResp {
            #[serde(rename = "exitCode", default)]
            exit_code: i64,
            #[serde(default)]
            result: String,
        }
        let r: ExecResp = parse_checked(resp).await?;
        Ok(ExecResult { exit_code: r.exit_code, stdout: r.result, stderr: String::new() })
    }

    async fn attach_pty(&self, _id: &SandboxId, _command: &[String], _opts: PtyOptions) -> Result<PtySession> {
        Err(self.not_supported("attach_pty"))
    }

    async fn connection_info(&self, id: &SandboxId) -> Result<ConnectionInfo> {
        Ok(ConnectionInfo {
            web_terminal_url: Some(format!("https://22222-{id}.proxy.daytona.work")),
            ssh_target: self.create_ssh_target(id).await.ok(),
        })
    }

    async fn put_file(&self, id: &SandboxId, path: &str, content: &[u8], _mode: u32) -> Result<()> {
        let form = reqwest::multipart::Form::new().text("files[0].path", path.to_string()).part(
            "files[0].file",
            reqwest::multipart::Part::bytes(content.to_vec())
                .file_name(path.to_string())
                .mime_str("application/octet-stream")
                .map_err(http_err)?,
        );
        let resp = self
            .req(reqwest::Method::POST, &format!("/toolbox/{id}/toolbox/files/bulk-upload"))
            .multipart(form)
            .send()
            .await
            .map_err(http_err)?;
        check_status(resp).await
    }

    async fn get_file(&self, id: &SandboxId, path: &str) -> Result<Vec<u8>> {
        let resp = self
            .req(reqwest::Method::GET, &format!("/toolbox/{id}/toolbox/files/download"))
            .query(&[("path", path)])
            .send()
            .await
            .map_err(http_err)?;
        let status = resp.status();
        let bytes = resp.bytes().await.map_err(http_err)?;
        if !status.is_success() {
            return Err(Error::Other(anyhow!("daytona download {}: {}", status, String::from_utf8_lossy(&bytes))));
        }
        Ok(bytes.to_vec())
    }

    async fn snapshot(&self, id: &SandboxId) -> Result<String> {
        let resp = self
            .req(reqwest::Method::POST, &format!("/sandbox/{id}/backup"))
            .send()
            .await
            .map_err(http_err)?;
        check_status(resp).await?;
        Ok(id.to_string())
    }

    async fn suspend(&self, id: &SandboxId) -> Result<()> {
        let resp = self.req(reqwest::Method::POST, &format!("/sandbox/{id}/stop")).send().await.map_err(http_err)?;
        check_status(resp).await
    }

    async fn resume(&self, id: &SandboxId) -> Result<()> {
        let resp = self.req(reqwest::Method::POST, &format!("/sandbox/{id}/start")).send().await.map_err(http_err)?;
        check_status(resp).await
    }

    async fn destroy(&self, id: &SandboxId) -> Result<()> {
        let resp = self
            .req(reqwest::Method::DELETE, &format!("/sandbox/{id}?force=true"))
            .send()
            .await
            .map_err(http_err)?;
        // Treat an already-gone sandbox as success.
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }
        check_status(resp).await
    }
}

/// Lenient view of a Daytona sandbox: only the fields we use; everything else
/// (including fields that come and go across API versions) is ignored.
#[derive(Deserialize)]
struct DaytonaSandbox {
    id: String,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    snapshot: Option<String>,
    #[serde(default)]
    labels: HashMap<String, String>,
    #[serde(default, rename = "createdAt")]
    created_at: Option<String>,
}

impl DaytonaSandbox {
    fn into_sandbox(self, id: SandboxId, provider_id: String) -> Sandbox {
        Sandbox {
            id,
            provider_id,
            status: map_state(self.state.as_deref().unwrap_or("")),
            image: self.snapshot.unwrap_or_default(),
            created_at: self.created_at.unwrap_or_default(),
            labels: self.labels,
        }
    }
}

fn http_err(e: reqwest::Error) -> Error {
    Error::Other(anyhow!("daytona request failed: {e}"))
}

/// Parse a JSON response, failing on a non-2xx status with the body for context.
async fn parse_checked<T: DeserializeOwned>(resp: reqwest::Response) -> Result<T> {
    let status = resp.status();
    let text = resp.text().await.map_err(http_err)?;
    if !status.is_success() {
        return Err(Error::Other(anyhow!("daytona {} : {}", status, truncate(&text))));
    }
    serde_json::from_str(&text)
        .map_err(|e| Error::Other(anyhow!("daytona response parse error: {e}; body: {}", truncate(&text))))
}

async fn check_status(resp: reqwest::Response) -> Result<()> {
    let status = resp.status();
    if status.is_success() {
        return Ok(());
    }
    let text = resp.text().await.unwrap_or_default();
    Err(Error::Other(anyhow!("daytona {} : {}", status, truncate(&text))))
}

fn truncate(s: &str) -> String {
    if s.len() > 400 {
        format!("{}...", &s[..400])
    } else {
        s.to_string()
    }
}

/// Map a Daytona state string to our status, leniently (case-insensitive).
fn map_state(state: &str) -> SandboxStatus {
    let s = state.to_lowercase();
    if s.contains("error") || s.contains("fail") {
        SandboxStatus::Error
    } else if s == "started" || s == "running" {
        SandboxStatus::Running
    } else if s.contains("stop") {
        SandboxStatus::Suspended
    } else if s.contains("archiv") || s.contains("destroy") {
        SandboxStatus::Stopped
    } else if s.is_empty() {
        SandboxStatus::Error
    } else {
        // creating, starting, restoring, pulling, building, pending, ...
        SandboxStatus::Creating
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
        format!("'{}'", arg.replace('\'', r#"'\''"#))
    }
}
