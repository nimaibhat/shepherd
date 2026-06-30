# Texting your agents from your phone (Telegram bridge)

The bridge lets you drive cloud sessions from your phone. Because the sandbox
already runs in the cloud and survives power-off, your phone only needs to reach
the bridge, never your laptop. A sandbox that auto-stopped is resumed
automatically when you text it.

It uses Telegram long polling, so it needs no public webhook or open ports, just
outbound HTTPS.

## 1. Create a bot

In Telegram, message [@BotFather](https://t.me/BotFather), send `/newbot`, and
copy the token it gives you.

## 2. Run the bridge on an always-on machine

The bridge is the one piece that must stay running for the phone flow to work.
Put it anywhere always-on (a small cloud VM, for example), not your laptop if you
want true power-off. The sandboxes themselves stay in Daytona regardless.

```sh
export TELEGRAM_BOT_TOKEN=...        # from BotFather
export SHEPHERD_PROVIDER=daytona     # or docker for local testing
export DAYTONA_API_KEY=...           # if using daytona
export ANTHROPIC_API_KEY=...
shepherd serve
```

## 3. Lock it to your chat

Text your bot anything. It replies with your chat id. Set that and restart so
only you can drive your agents:

```sh
export TELEGRAM_ALLOWED_CHATS=123456789
shepherd serve
```

## 4. Use it

- `/ls` - list your sessions and their live status
- `/use <session-id>` - bind this chat to a session
- then just text - each message is sent to the agent as a turn; the cloud
  sandbox is woken automatically if it had auto-stopped, and the agent's reply
  comes back to the chat

Sessions are created from a computer with `shepherd run --agent ...` (it needs
your repo); once a session exists, you drive it entirely from your phone.

## Notes and limits

- One turn at a time per bridge: a long agent run holds the loop until it
  replies. Fine for a single user; concurrency is a later improvement.
- Replies are truncated to Telegram's message limit; full output is still in the
  sandbox (use `shepherd logs` from a computer for everything).
- Waking an archived sandbox (auto-archived after a day) takes a little longer
  than waking a stopped one, but neither needs your laptop.
