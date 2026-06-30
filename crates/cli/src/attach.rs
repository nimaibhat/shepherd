//! Interactive attach: stream a real terminal from inside the sandbox to the
//! local terminal, and detach without killing the box (PLAN.md M6). Because the
//! box is a long lived keep-alive container and each attach is a fresh exec,
//! detaching ends the local shell but the workspace and box persist, so you can
//! reattach and pick up where you left off.

use std::collections::HashMap;

use anyhow::{bail, Result};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, size};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::oneshot;

use shepherd_core::ids::SessionId;
use shepherd_core::sandbox::{PtyControl, PtyOptions, SandboxProvider};

use crate::store::Store;

/// Ctrl-] detaches, leaving the sandbox running.
const DETACH_BYTE: u8 = 0x1d;

pub async fn attach(store: &Store, provider: &dyn SandboxProvider, session: &str) -> Result<()> {
    let id: SessionId = session.into();
    let Some(s) = store.get(&id)? else {
        bail!("no such session: {session}");
    };
    let Some(sandbox_id) = s.sandbox_id.clone() else {
        bail!("session {session} has no sandbox");
    };
    let mount = s.workspace.mount_path().to_string();

    let (cols, rows) = size().unwrap_or((80, 24));
    let mut env = HashMap::new();
    env.insert("TERM".to_string(), "xterm-256color".to_string());

    let pty = provider
        .attach_pty(
            &sandbox_id,
            &["/bin/sh".to_string()],
            PtyOptions {
                cwd: Some(mount.clone()),
                env,
                cols: Some(cols),
                rows: Some(rows),
            },
        )
        .await?;

    println!("attached to {session} ({sandbox_id}) at {mount}. detach with Ctrl-]");

    enable_raw_mode()?;
    let outcome = pump(pty).await;
    let _ = disable_raw_mode();
    println!();

    match outcome {
        Outcome::Detached => println!("detached. sandbox still running; reattach with: shepherd attach {session}"),
        Outcome::ShellExited(code) => println!("shell exited ({code}). sandbox still running."),
        Outcome::Closed => println!("connection closed. sandbox still running."),
    }
    Ok(())
}

enum Outcome {
    Detached,
    ShellExited(i64),
    Closed,
}

/// Wire stdin, stdout, resize, and exit to the pty until detach or shell exit.
async fn pump(pty: shepherd_core::sandbox::PtySession) -> Outcome {
    let input = pty.input;
    let mut output = pty.output;
    let control = pty.control;
    let mut exit = pty.exit;

    // stdin -> pty, watching for the detach byte.
    let (detach_tx, mut detach_rx) = oneshot::channel::<()>();
    let input_task = tokio::spawn(async move {
        let mut stdin = tokio::io::stdin();
        let mut buf = [0u8; 1024];
        let mut detach_tx = Some(detach_tx);
        loop {
            match stdin.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    let chunk = &buf[..n];
                    if let Some(pos) = chunk.iter().position(|&b| b == DETACH_BYTE) {
                        if pos > 0 {
                            let _ = input.send(chunk[..pos].to_vec()).await;
                        }
                        if let Some(tx) = detach_tx.take() {
                            let _ = tx.send(());
                        }
                        break;
                    }
                    if input.send(chunk.to_vec()).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // SIGWINCH -> resize the pty.
    let resize_task = tokio::spawn(async move {
        let Ok(mut sigwinch) = signal(SignalKind::window_change()) else {
            return;
        };
        while sigwinch.recv().await.is_some() {
            if let Ok((cols, rows)) = size() {
                let _ = control.send(PtyControl::Resize { cols, rows }).await;
            }
        }
    });

    let mut stdout = tokio::io::stdout();
    let outcome = loop {
        tokio::select! {
            chunk = output.recv() => match chunk {
                Some(bytes) => {
                    let _ = stdout.write_all(&bytes).await;
                    let _ = stdout.flush().await;
                }
                None => break Outcome::Closed,
            },
            code = &mut exit => break Outcome::ShellExited(code.unwrap_or(-1)),
            _ = &mut detach_rx => break Outcome::Detached,
        }
    };

    input_task.abort();
    resize_task.abort();
    outcome
}
