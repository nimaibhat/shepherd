//! SandboxProvider backed by local Docker via bollard. Lets the entire Shepherd
//! flow run with zero cloud cost or accounts (PLAN.md section 7). Idle
//! persistence is mapped onto Docker: suspend = pause, resume = unpause,
//! snapshot = commit.

use std::collections::HashMap;

use anyhow::{anyhow, Context};
use async_trait::async_trait;
use bollard::container::{
    Config, CreateContainerOptions, DownloadFromContainerOptions, ListContainersOptions,
    RemoveContainerOptions, UploadToContainerOptions,
};
use bollard::exec::{CreateExecOptions, ResizeExecOptions, StartExecResults};
use bollard::image::{CommitContainerOptions, CreateImageOptions};
use bollard::models::HostConfig;
use bollard::Docker;
use bytes::Bytes;
use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, oneshot};

use shepherd_core::errors::{Error, Result};
use shepherd_core::ids::SandboxId;
use shepherd_core::sandbox::{
    ExecOptions, ExecResult, PtyControl, PtyOptions, PtySession, Sandbox, SandboxProvider,
    SandboxSpec, SandboxStatus,
};

use super::tar::{split_container_path, tar_single_file, untar_first_file};

const MANAGED_LABEL: &str = "shepherd.managed";
const ID_LABEL: &str = "shepherd.id";

/// SandboxProvider backed by the local Docker daemon.
pub struct DockerProvider {
    docker: Docker,
    keep_alive_cmd: Vec<String>,
}

impl DockerProvider {
    /// Connect using the local Docker defaults (DOCKER_HOST or the default socket).
    pub fn connect() -> Result<Self> {
        let docker = Docker::connect_with_local_defaults()
            .map_err(|e| Error::Other(anyhow!("connect to docker: {e}")))?;
        Ok(Self {
            docker,
            keep_alive_cmd: vec!["sleep".into(), "infinity".into()],
        })
    }

    async fn ensure_image(&self, image: &str) -> Result<()> {
        if self.docker.inspect_image(image).await.is_ok() {
            return Ok(());
        }
        // Split a "repo:tag" reference, defaulting the tag to latest. Guard
        // against a registry port (host:5000/img has no real tag).
        let (from_image, tag) = match image.rsplit_once(':') {
            Some((name, tag)) if !tag.contains('/') => (name.to_string(), tag.to_string()),
            _ => (image.to_string(), "latest".to_string()),
        };
        let opts = CreateImageOptions {
            from_image,
            tag,
            ..Default::default()
        };
        let mut stream = self.docker.create_image(Some(opts), None, None);
        while let Some(item) = stream.next().await {
            item.map_err(|e| Error::Other(anyhow!("pull image {image}: {e}")))?;
        }
        Ok(())
    }

    /// Resolve our SandboxId to the underlying docker container id.
    async fn container_id(&self, id: &SandboxId) -> Result<Option<String>> {
        let mut filters = HashMap::new();
        filters.insert("label".to_string(), vec![format!("{ID_LABEL}={id}")]);
        let opts = ListContainersOptions {
            all: true,
            filters,
            ..Default::default()
        };
        let list = self
            .docker
            .list_containers(Some(opts))
            .await
            .map_err(boxed)?;
        Ok(list.into_iter().next().and_then(|c| c.id))
    }

    async fn require_container(&self, id: &SandboxId) -> Result<String> {
        self.container_id(id)
            .await?
            .ok_or_else(|| Error::SandboxNotFound(id.to_string()))
    }
}

#[async_trait]
impl SandboxProvider for DockerProvider {
    fn id(&self) -> String {
        "docker".to_string()
    }

    async fn create(&self, spec: SandboxSpec) -> Result<Sandbox> {
        self.ensure_image(&spec.image).await?;
        let id = SandboxId::new();

        let mut labels = spec.labels.clone();
        labels.insert(MANAGED_LABEL.to_string(), "true".to_string());
        labels.insert(ID_LABEL.to_string(), id.to_string());

        let env: Vec<String> = spec.env.iter().map(|(k, v)| format!("{k}={v}")).collect();

        let host_config = HostConfig {
            memory: spec.resources.memory_mb.map(|m| (m * 1024 * 1024) as i64),
            nano_cpus: spec.resources.cpus.map(|c| (c * 1e9) as i64),
            ..Default::default()
        };

        let config = Config {
            image: Some(spec.image.clone()),
            // Override the entrypoint (not just cmd) so the box stays alive
            // regardless of the base image's ENTRYPOINT; we exec into it.
            entrypoint: Some(self.keep_alive_cmd.clone()),
            cmd: None,
            labels: Some(labels.clone()),
            env: Some(env),
            tty: Some(false),
            host_config: Some(host_config),
            ..Default::default()
        };

        let name = format!("shepherd-{id}");
        self.docker
            .create_container(Some(CreateContainerOptions { name: name.clone(), platform: None }), config)
            .await
            .map_err(boxed)?;
        self.docker
            .start_container::<String>(&name, None)
            .await
            .map_err(boxed)?;

        self.get(&id)
            .await?
            .ok_or_else(|| Error::SandboxNotFound(id.to_string()))
    }

    async fn get(&self, id: &SandboxId) -> Result<Option<Sandbox>> {
        let Some(cid) = self.container_id(id).await? else {
            return Ok(None);
        };
        let info = self.docker.inspect_container(&cid, None).await.map_err(boxed)?;
        let status = info
            .state
            .as_ref()
            .map(map_inspect_state)
            .unwrap_or(SandboxStatus::Error);
        let image = info
            .config
            .as_ref()
            .and_then(|c| c.image.clone())
            .unwrap_or_default();
        let created_at = info.created.unwrap_or_default();
        let labels = info
            .config
            .and_then(|c| c.labels)
            .unwrap_or_default();
        Ok(Some(Sandbox {
            id: id.clone(),
            provider_id: self.id(),
            status,
            image,
            created_at,
            labels,
        }))
    }

    async fn list(&self, labels: &HashMap<String, String>) -> Result<Vec<Sandbox>> {
        let mut label_filters = vec![format!("{MANAGED_LABEL}=true")];
        for (k, v) in labels {
            label_filters.push(format!("{k}={v}"));
        }
        let mut filters = HashMap::new();
        filters.insert("label".to_string(), label_filters);
        let opts = ListContainersOptions {
            all: true,
            filters,
            ..Default::default()
        };
        let list = self.docker.list_containers(Some(opts)).await.map_err(boxed)?;

        let mut out = Vec::new();
        for c in list {
            let labels = c.labels.unwrap_or_default();
            let id: SandboxId = labels.get(ID_LABEL).cloned().unwrap_or_default().into();
            let created_at = c
                .created
                .and_then(|secs| chrono::DateTime::from_timestamp(secs, 0))
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_default();
            out.push(Sandbox {
                id,
                provider_id: self.id(),
                status: map_summary_state(c.state.as_deref().unwrap_or("")),
                image: c.image.unwrap_or_default(),
                created_at,
                labels,
            });
        }
        Ok(out)
    }

    async fn exec(&self, id: &SandboxId, command: &[String], opts: ExecOptions) -> Result<ExecResult> {
        let cid = self.require_container(id).await?;
        let env: Vec<String> = opts.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        let exec = self
            .docker
            .create_exec(
                &cid,
                CreateExecOptions {
                    cmd: Some(command.to_vec()),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    working_dir: opts.cwd.clone(),
                    env: if env.is_empty() { None } else { Some(env) },
                    ..Default::default()
                },
            )
            .await
            .map_err(boxed)?;

        let mut stdout = String::new();
        let mut stderr = String::new();
        if let StartExecResults::Attached { mut output, .. } =
            self.docker.start_exec(&exec.id, None).await.map_err(boxed)?
        {
            while let Some(item) = output.next().await {
                match item.map_err(boxed)? {
                    bollard::container::LogOutput::StdOut { message } => {
                        stdout.push_str(&String::from_utf8_lossy(&message));
                    }
                    bollard::container::LogOutput::StdErr { message } => {
                        stderr.push_str(&String::from_utf8_lossy(&message));
                    }
                    other => stdout.push_str(&String::from_utf8_lossy(&other.into_bytes())),
                }
            }
        }

        let inspect = self.docker.inspect_exec(&exec.id).await.map_err(boxed)?;
        Ok(ExecResult {
            exit_code: inspect.exit_code.unwrap_or(-1),
            stdout,
            stderr,
        })
    }

    async fn attach_pty(&self, id: &SandboxId, command: &[String], opts: PtyOptions) -> Result<PtySession> {
        let cid = self.require_container(id).await?;
        let env: Vec<String> = opts.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        let exec = self
            .docker
            .create_exec(
                &cid,
                CreateExecOptions {
                    cmd: Some(command.to_vec()),
                    attach_stdin: Some(true),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    tty: Some(true),
                    working_dir: opts.cwd.clone(),
                    env: if env.is_empty() { None } else { Some(env) },
                    ..Default::default()
                },
            )
            .await
            .map_err(boxed)?;

        let StartExecResults::Attached { mut output, mut input } =
            self.docker.start_exec(&exec.id, None).await.map_err(boxed)?
        else {
            return Err(Error::Other(anyhow!("docker returned a detached exec")));
        };

        if let (Some(cols), Some(rows)) = (opts.cols, opts.rows) {
            let _ = self
                .docker
                .resize_exec(&exec.id, ResizeExecOptions { height: rows, width: cols })
                .await;
        }

        let (in_tx, mut in_rx) = mpsc::channel::<Vec<u8>>(64);
        let (out_tx, out_rx) = mpsc::channel::<Vec<u8>>(256);
        let (ctrl_tx, mut ctrl_rx) = mpsc::channel::<PtyControl>(16);
        let (exit_tx, exit_rx) = oneshot::channel::<i64>();

        let docker = self.docker.clone();
        let exec_id = exec.id.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    maybe_in = in_rx.recv() => match maybe_in {
                        Some(bytes) => {
                            if input.write_all(&bytes).await.is_err() { break; }
                            let _ = input.flush().await;
                        }
                        None => break,
                    },
                    maybe_ctrl = ctrl_rx.recv() => match maybe_ctrl {
                        Some(PtyControl::Resize { cols, rows }) => {
                            let _ = docker
                                .resize_exec(&exec_id, ResizeExecOptions { height: rows, width: cols })
                                .await;
                        }
                        Some(PtyControl::Kill) | None => break,
                    },
                    maybe_out = output.next() => match maybe_out {
                        Some(Ok(chunk)) => {
                            if out_tx.send(chunk.into_bytes().to_vec()).await.is_err() { break; }
                        }
                        Some(Err(_)) | None => break,
                    },
                }
            }
            let code = docker
                .inspect_exec(&exec_id)
                .await
                .ok()
                .and_then(|i| i.exit_code)
                .unwrap_or(-1);
            let _ = exit_tx.send(code);
        });

        Ok(PtySession {
            input: in_tx,
            output: out_rx,
            control: ctrl_tx,
            exit: exit_rx,
        })
    }

    async fn put_file(&self, id: &SandboxId, path: &str, content: &[u8], mode: u32) -> Result<()> {
        let cid = self.require_container(id).await?;
        let (dir, base) = split_container_path(path);
        let archive = tar_single_file(&base, content, mode)
            .context("build tar")
            .map_err(Error::Other)?;
        self.docker
            .upload_to_container(
                &cid,
                Some(UploadToContainerOptions { path: dir, ..Default::default() }),
                Bytes::from(archive),
            )
            .await
            .map_err(boxed)?;
        Ok(())
    }

    async fn get_file(&self, id: &SandboxId, path: &str) -> Result<Vec<u8>> {
        let cid = self.require_container(id).await?;
        let mut stream = self.docker.download_from_container(
            &cid,
            Some(DownloadFromContainerOptions { path: path.to_string() }),
        );
        let mut buf = Vec::new();
        while let Some(item) = stream.next().await {
            buf.extend_from_slice(&item.map_err(boxed)?);
        }
        untar_first_file(&buf).map_err(Error::Other)
    }

    async fn snapshot(&self, id: &SandboxId) -> Result<String> {
        let cid = self.require_container(id).await?;
        let opts = CommitContainerOptions {
            container: cid,
            repo: "shepherd-snapshot".to_string(),
            tag: id.to_string(),
            ..Default::default()
        };
        let res = self
            .docker
            .commit_container(opts, Config::<String>::default())
            .await
            .map_err(boxed)?;
        Ok(res.id.unwrap_or_default())
    }

    async fn suspend(&self, id: &SandboxId) -> Result<()> {
        let cid = self.require_container(id).await?;
        self.docker.pause_container(&cid).await.map_err(boxed)?;
        Ok(())
    }

    async fn resume(&self, id: &SandboxId) -> Result<()> {
        let cid = self.require_container(id).await?;
        self.docker.unpause_container(&cid).await.map_err(boxed)?;
        Ok(())
    }

    async fn destroy(&self, id: &SandboxId) -> Result<()> {
        let Some(cid) = self.container_id(id).await? else {
            return Ok(());
        };
        self.docker
            .remove_container(&cid, Some(RemoveContainerOptions { force: true, ..Default::default() }))
            .await
            .map_err(boxed)?;
        Ok(())
    }
}

/// Map a bollard error into our error type.
fn boxed(e: bollard::errors::Error) -> Error {
    Error::Other(anyhow!(e.to_string()))
}

fn map_summary_state(state: &str) -> SandboxStatus {
    match state {
        "running" => SandboxStatus::Running,
        "paused" => SandboxStatus::Suspended,
        "created" => SandboxStatus::Creating,
        "exited" | "dead" | "removing" => SandboxStatus::Stopped,
        _ => SandboxStatus::Error,
    }
}

fn map_inspect_state(state: &bollard::models::ContainerState) -> SandboxStatus {
    if state.paused == Some(true) {
        return SandboxStatus::Suspended;
    }
    if state.running == Some(true) {
        return SandboxStatus::Running;
    }
    if state.dead == Some(true) {
        return SandboxStatus::Stopped;
    }
    match state.status {
        Some(bollard::models::ContainerStateStatusEnum::CREATED) => SandboxStatus::Creating,
        Some(bollard::models::ContainerStateStatusEnum::EXITED) => SandboxStatus::Stopped,
        _ => SandboxStatus::Error,
    }
}
