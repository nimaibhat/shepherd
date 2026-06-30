//! Messaging bridge (PLAN.md M11): drive cloud agents from your phone over
//! Telegram. Because the sandbox already runs in the cloud and survives
//! power-off, the phone only needs to reach this bridge, never your laptop.
//!
//! Uses Telegram long polling (getUpdates), so it needs no public webhook or
//! open ports: just outbound HTTPS. Run it on any always-on machine
//! (`shepherd serve`); a slept sandbox is resumed on demand when you text it.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;

use shepherd_agent::ClaudeRunner;
use shepherd_core::agent::{AgentEvent, AgentRunner, RunRequest};
use shepherd_core::ids::SessionId;
use shepherd_core::sandbox::{SandboxProvider, SandboxStatus};

use crate::store::Store;

const TELEGRAM_API: &str = "https://api.telegram.org";
const MAX_REPLY: usize = 3900; // Telegram caps messages at 4096 chars.

pub async fn run_bot(store: &Store, provider: &dyn SandboxProvider) -> Result<()> {
    let token = std::env::var("TELEGRAM_BOT_TOKEN")
        .context("set TELEGRAM_BOT_TOKEN (create a bot with @BotFather)")?;
    let allowed: Vec<i64> = std::env::var("TELEGRAM_ALLOWED_CHATS")
        .unwrap_or_default()
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();

    let client = reqwest::Client::new();
    let mut offset: i64 = 0;
    let mut bindings: HashMap<i64, SessionId> = HashMap::new();

    println!("shepherd bot: polling Telegram (provider {})", provider.id());
    if allowed.is_empty() {
        println!("note: TELEGRAM_ALLOWED_CHATS is unset; text the bot once and it will reply with your chat id, then set it and restart.");
    }

    loop {
        let updates = match get_updates(&client, &token, offset).await {
            Ok(u) => u,
            Err(e) => {
                eprintln!("getUpdates failed: {e}; retrying");
                tokio::time::sleep(Duration::from_secs(3)).await;
                continue;
            }
        };
        for update in updates {
            offset = update.update_id + 1;
            let Some(msg) = update.message else { continue };
            let (Some(text), chat) = (msg.text, msg.chat.id) else { continue };
            if let Err(e) =
                handle(store, provider, &client, &token, &allowed, &mut bindings, chat, text.trim())
                    .await
            {
                let _ = send(&client, &token, chat, &format!("error: {e}")).await;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle(
    store: &Store,
    provider: &dyn SandboxProvider,
    client: &reqwest::Client,
    token: &str,
    allowed: &[i64],
    bindings: &mut HashMap<i64, SessionId>,
    chat: i64,
    text: &str,
) -> Result<()> {
    // Authorize. Until an allowlist is configured, reveal the chat id so the
    // operator can lock the bridge to their own chat, but do nothing else.
    if allowed.is_empty() {
        send(client, token, chat, &format!(
            "Your chat id is {chat}.\nSet TELEGRAM_ALLOWED_CHATS={chat} and restart shepherd serve to enable the bridge."
        )).await?;
        return Ok(());
    }
    if !allowed.contains(&chat) {
        return Ok(()); // silently ignore strangers
    }

    if text == "/start" || text == "/help" {
        send(client, token, chat, HELP).await?;
        return Ok(());
    }

    if text == "/ls" {
        let sessions = store.list()?;
        let mut out = String::from("sessions:\n");
        if sessions.is_empty() {
            out.push_str("  (none) - create one with: shepherd run --agent ...");
        }
        for s in sessions {
            let live = match &s.sandbox_id {
                Some(id) => provider.get(id).await.ok().flatten().map(|sb| format!("{:?}", sb.status)).unwrap_or_else(|| "gone".into()),
                None => "-".into(),
            };
            out.push_str(&format!("  {} [{}] {}\n", s.id, live, s.title));
        }
        send(client, token, chat, &out).await?;
        return Ok(());
    }

    if let Some(rest) = text.strip_prefix("/use ") {
        let id: SessionId = rest.trim().into();
        match store.get(&id)? {
            Some(_) => {
                bindings.insert(chat, id.clone());
                send(client, token, chat, &format!("bound this chat to {id}. just text to send the agent a turn.")).await?;
            }
            None => send(client, token, chat, &format!("no such session: {rest}")).await?,
        }
        return Ok(());
    }

    // Plain text: inject as an agent turn into the bound session.
    let Some(session_id) = bindings.get(&chat).cloned() else {
        send(client, token, chat, "no session bound. send /ls then /use <session-id>.").await?;
        return Ok(());
    };
    let Some(mut session) = store.get(&session_id)? else {
        send(client, token, chat, "that session is gone. /ls to pick another.").await?;
        return Ok(());
    };
    let Some(sandbox_id) = session.sandbox_id.clone() else {
        send(client, token, chat, "session has no sandbox.").await?;
        return Ok(());
    };
    let mount = session.workspace.mount_path().to_string();

    // Wake the box if it auto-stopped. No laptop required: this is a cloud call.
    if let Ok(Some(sb)) = provider.get(&sandbox_id).await {
        if matches!(sb.status, SandboxStatus::Suspended | SandboxStatus::Stopped) {
            send(client, token, chat, "waking the sandbox...").await?;
            provider.resume(&sandbox_id).await?;
        }
    }
    send(client, token, chat, "working...").await?;

    // Run one agent turn and collect its text for the reply.
    let runner = ClaudeRunner::default();
    let req = RunRequest {
        sandbox_id: sandbox_id.clone(),
        prompt: text.to_string(),
        cwd: mount,
        resume_agent_session_id: session.agent_session_id.clone(),
        allowed_tools: Vec::new(),
        env: HashMap::new(),
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel::<AgentEvent>(1024);
    let consumer = tokio::spawn(async move {
        let mut texts = Vec::new();
        let mut error = None;
        while let Some(ev) = rx.recv().await {
            match ev {
                AgentEvent::Text { text } => texts.push(text),
                AgentEvent::Error { message } => error = Some(message),
                _ => {}
            }
        }
        (texts, error)
    });
    let result = runner.run(provider, req, tx).await;
    let (texts, error) = consumer.await.unwrap_or_default();

    match result {
        Ok(run) => {
            if session.agent_session_id.is_none() {
                session.agent_session_id = run.agent_session_id;
                store.upsert(&session).ok();
            }
            let reply = if let Some(e) = error {
                format!("agent error: {e}")
            } else if texts.is_empty() {
                "(the agent produced no text this turn)".to_string()
            } else {
                texts.join("\n")
            };
            send(client, token, chat, &truncate(&reply)).await?;
        }
        Err(e) => send(client, token, chat, &format!("run failed: {e}")).await?,
    }
    Ok(())
}

const HELP: &str = "shepherd bridge. commands:\n\
/ls - list sessions and status\n\
/use <session-id> - bind this chat to a session\n\
then just text to send the agent a turn. the cloud sandbox is woken automatically.";

fn truncate(s: &str) -> String {
    if s.len() <= MAX_REPLY {
        s.to_string()
    } else {
        format!("{}\n...(truncated)", &s[..MAX_REPLY])
    }
}

// --- minimal Telegram Bot API client (long polling) ---

#[derive(Deserialize)]
struct TgResponse<T> {
    result: Option<T>,
}

#[derive(Deserialize)]
struct Update {
    update_id: i64,
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    chat: Chat,
    text: Option<String>,
}

#[derive(Deserialize)]
struct Chat {
    id: i64,
}

async fn get_updates(client: &reqwest::Client, token: &str, offset: i64) -> Result<Vec<Update>> {
    let url = format!("{TELEGRAM_API}/bot{token}/getUpdates");
    let resp = client
        .get(&url)
        .query(&[("offset", offset.to_string()), ("timeout", "30".to_string())])
        .timeout(Duration::from_secs(40))
        .send()
        .await?
        .error_for_status()?;
    let body: TgResponse<Vec<Update>> = resp.json().await?;
    Ok(body.result.unwrap_or_default())
}

async fn send(client: &reqwest::Client, token: &str, chat: i64, text: &str) -> Result<()> {
    let url = format!("{TELEGRAM_API}/bot{token}/sendMessage");
    client
        .post(&url)
        .json(&serde_json::json!({ "chat_id": chat, "text": text }))
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}
