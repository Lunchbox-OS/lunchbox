# shepherd-ipc

IPC layer for Shepherd.

## Overview

This crate provides the local inter-process communication infrastructure between the Shepherd service (`shepherdd`) and its clients (launcher UI, HUD overlay, admin tools). It includes:

- **Unix domain socket server** - Listens for client connections
- **NDJSON protocol** - Newline-delimited JSON message framing
- **Client management** - Connection tracking and cleanup
- **Peer authentication** - cgroup-based allow-list (issue #144)
- **Event broadcasting** - Push events to subscribed clients

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│                      shepherdd                          │
│  ┌──────────────────────────────────────────────────┐   │
│  │                  IpcServer                       │   │
│  │  ┌──────────┐ ┌─────────┐ ┌─────────┐            │   │
│  │  │Client 1  │ │Client 2 │ │Client 3 │ ...        │   │
│  │  │(Launcher)│ │ (HUD)   │ │ (Admin) │            │   │
│  │  └────┬─────┘ └────┬────┘ └────┬────┘            │   │
│  │       │            │           │                 │   │
│  │       └────────────┴───────────┘                 │   │
│  │              Unix Domain Socket                  │   │
│  └──────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────┘
         │              │              │
    ┌────┴────┐    ┌────┴────┐    ┌────┴────┐
    │Launcher │    │   HUD   │    │  Admin  │
    │   UI    │    │ Overlay │    │  Tool   │
    └─────────┘    └─────────┘    └─────────┘
```

## Server Usage

### Starting the Server

```rust
use shepherd_ipc::IpcServer;

let mut server = IpcServer::new("/run/shepherdd/shepherdd.sock");
server.start().await?;

// Get message receiver for the main loop
let mut messages = server.take_message_receiver().await.unwrap();

// Accept connections in background
tokio::spawn(async move {
    server.run().await
});

// Process messages in main loop
while let Some(msg) = messages.recv().await {
    match msg {
        ServerMessage::Request { client_id, request } => {
            // Handle request, send response
            let response = handle_request(request);
            server.send_response(&client_id, response).await?;
        }
        ServerMessage::ClientConnected { client_id, info } => {
            println!("Client {} connected as {:?}", client_id, info.role);
        }
        ServerMessage::ClientDisconnected { client_id } => {
            println!("Client {} disconnected", client_id);
        }
    }
}
```

### Broadcasting Events

```rust
use shepherd_api::Event;

// Send to all subscribed clients
server.broadcast_event(Event::new(EventPayload::StateChanged(snapshot))).await;
```

### Which peers are accepted (issue #144)

The peer's uid decides nothing, because every activity `shepherdd` launches runs
as `shepherdd`'s own uid. The identity that does separate them is the peer's
**cgroup**: the kernel maintains it, every descendant inherits it — including
double-forked and reparented ones, where a PPID walk falls apart — and an
unprivileged process can neither forge it nor climb out of it.

| Peer | Verdict |
|------|---------|
| In `shepherdd`'s own cgroup | accepted as `Admin` |
| root, from any cgroup (`sudo`) | accepted as `Admin` |
| Anything else, or anything that cannot be identified | **refused at accept** |

What is in `shepherdd`'s cgroup is worth stating in full, because that set *is*
the trust boundary: sway, `shepherdd`, the launcher, the HUD, `swayidle` and the
one-shots sway starts for a keybinding — and also shepherd's own helper
subprocesses, which are children of the daemon: the input-compat sidecars,
`wl-mirror`, the pairing overlay, and the short-lived query commands
(`wpctl`/`pactl`/`amixer`, `pw-dump`, `brightnessctl`, `pgrep`, `pkcheck`).

Activities are not, by construction — see `shepherd-host-linux`'s README. Nor is
`yt-dlp`, which is scoped out of this cgroup despite being shepherd's own
subprocess, because it parses remote input on a background timer.

The decision is made **once per connection, at accept**, not per call: one
decision instead of many, it cannot be forgotten when a method is added, and a
peer that should not read state at all never reaches the event stream. A refusal
just closes the connection — a refusal that answers is a refusal that can be
probed — and reports `ServerMessage::ClientRejected` so the daemon can raise a
diagnostic.

It is an **allow-list**, not a deny-list. "Refuse peers I recognise as
activities" fails open on exactly the cases it cannot classify, and there is a
verified escape that lands in that gap: an activity can ask `systemd --user` to
start a process for it in a cgroup that is in no shepherd scope at all. That
process is refused here because it is not *in `shepherdd`'s cgroup*, which is a
different question from whether it is in a scope shepherd made.

`PeerPolicy::unrestricted()` restores the old uid-only classification; the
daemon uses it for `--no-restrict-ipc-peers`. See `src/peer.rs` for how the
cgroup is read (`SO_PEERPIDFD` + `PIDFD_GET_INFO`, so no `/proc` lookup and no
pid-reuse race) and `docs/ai/history/2026-08-29 001` / `002` for the design and
the measurements behind it.

`ClientRole` still rides on `ClientInfo` and is recorded in the audit log, but
nothing consults it at dispatch: once the allow-list is in place every accepted
peer is either root or shepherd's own code, so a per-method tier split would
have no security content to enforce.

### Exercising the armed check without a device

`PeerPolicy::restricted()` degrades wherever shepherd's cgroup is one an
activity could join, which is every stack started from a shell — so a dev
session never runs the enforced path. A **system**-manager scope owned by the
right uid is not delegated, which is structurally what a logind session scope
is, so this reaches it:

```sh
sudo systemd-run --uid=1000 --gid=1000 --scope --slice=user-1000.slice \
  --setenv=XDG_RUNTIME_DIR=/run/user/1000 \
  --setenv=DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus \
  ./target/debug/shepherdd -c ./config.example.toml -s /tmp/t/s.sock -d /tmp/t/data
```

Both `--setenv`s matter: without them `systemd-run --user --scope` cannot reach
the user bus, so activities cannot be isolated and the daemon refuses to arm —
correctly, since the check separates nothing when everything shares its cgroup.
Look for `Management socket accepts only this session and root`; any other line
means it degraded, and says why.

### Which daemon a client will talk to (issue #144)

The same question backwards, and it needs asking. The socket lives in a
directory owned by the uid every activity runs as, so an activity can
`unlink()` it and `bind()` its own listener at the same path: established
connections survive, but every new one — a relaunched launcher or HUD, a
keybinding one-shot, `swayidle`'s screen blank — arrives at the impostor.

**No file mode prevents this**, which is worth stating because two fixes suggest
themselves and both were measured and rejected:

| Attempt | Result |
|---------|--------|
| Socket in a root-owned directory | `shepherdd` cannot bind there at all — `EACCES` |
| Sticky bit on the directory | Restricts deletion to the file's *owner*, and an activity **is** the owner: it shares the daemon's uid |

Preventing the name being taken needs the socket to be created by something
that is not the daemon — systemd socket activation, where root binds it and
passes the fd — which `shepherdd` cannot use while sway `exec`s it.

So the client checks who answered, exactly as the server checks who called.
`IpcClient::connect` compares the listener's cgroup with its own via
`classify_server`:

| What answered | Verdict |
|---------------|---------|
| A process in our own cgroup — the session's real daemon | accepted |
| Anything in another cgroup | **refused** |
| A listener that cannot be identified | **refused** — an impostor can cause this by exiting once the connection is accepted |
| Our own cgroup unreadable | accepted, with a warning — nothing an activity does causes this, and refusing would leave a device with a launcher that will not start |

Root is exempt, because `sudo` reaches the daemon from a login session that is
never shepherd's cgroup — the same exemption the server makes.
`IpcClient::connect_unverified` exists for clients that legitimately live
outside the session; the launcher, the HUD and the one-shots must never use it,
since they are precisely the clients an impostor is worth deceiving.

A refusal is reported at most once a minute (`RejectionReporter`). The first
is always reported in full — a single probe is never silent — and the rest are
counted, with the tally carried on the next report. Without that, an activity
could call `connect()` in a loop: each refusal is a `warn!` line plus a
diagnostic whose text names the peer's cgroup, and a diagnostic whose text has
changed wakes every subscriber — the web UI, the companion app, the launcher.
Nothing is breached, but it would be noise an activity controls, aimed at the
channel an administrator watches for exactly this warning.

The daemon also notices: `IpcServer::socket_was_replaced` compares
`(st_dev, st_ino)` against what it bound, and `shepherdd` polls it once a minute
and raises the `Critical` diagnostic `ipc_socket_replaced`. Nothing is given
away when this happens — clients refuse the impostor — but the session has
become unreachable, and that should not look like a launcher that stopped
working for no reason.

## Client Usage

### Connecting

```rust
use shepherd_ipc::IpcClient;

let mut client = IpcClient::connect("/run/shepherdd/shepherdd.sock").await?;
```

### Sending Commands

```rust
use shepherd_api::{Command, Response};

// Request current state
client.send(Command::GetState).await?;
let response: Response = client.recv().await?;

// Launch an entry
client.send(Command::Launch { 
    entry_id: "minecraft".into() 
}).await?;
let response = client.recv().await?;
```

### Subscribing to Events

```rust
// Subscribe to event stream
client.send(Command::SubscribeEvents).await?;

// Receive events
loop {
    match client.recv_event().await {
        Ok(event) => {
            match event.payload {
                EventPayload::WarningIssued { remaining, .. } => {
                    println!("Warning: {} seconds remaining", remaining.as_secs());
                }
                EventPayload::SessionEnded { .. } => {
                    println!("Session ended");
                }
                _ => {}
            }
        }
        Err(IpcError::ConnectionClosed) => break,
        Err(e) => eprintln!("Error: {}", e),
    }
}
```

## Protocol

### Message Format

Messages use NDJSON (newline-delimited JSON):

```
{"type":"request","id":1,"command":"get_state"}\n
{"type":"response","id":1,"payload":{"api_version":1,...}}\n
{"type":"event","payload":{"type":"state_changed",...}}\n
```

### Request/Response

Each request has an ID, matched in the response:

```json
// Request
{"type":"request","id":42,"command":{"type":"launch","entry_id":"minecraft"}}

// Response
{"type":"response","id":42,"success":true,"payload":{...}}
```

### Events

Events are pushed without request IDs:

```json
{"type":"event","payload":{"type":"warning_issued","threshold":60,"remaining":{"secs":60}}}
```

## Socket Permissions

The socket is created with mode `0660`:
- Owner can read/write
- Group can read/write
- Others have no access

This allows the service to run as a dedicated user while permitting group members (e.g., `shepherd` group) to connect.

## Rate Limiting

Per-client rate limiting prevents buggy or malicious clients from overwhelming the service:

```rust
// Default: 10 commands per second per client
if rate_limiter.check(&client_id) {
    // Process command
} else {
    // Respond with rate limit error
}
```

## Error Handling

```rust
use shepherd_ipc::IpcError;

match result {
    Err(IpcError::ConnectionClosed) => {
        // Client disconnected
    }
    Err(IpcError::Json(e)) => {
        // Protocol error
    }
    Err(IpcError::Io(e)) => {
        // Socket error
    }
    _ => {}
}
```

## Dependencies

- `tokio` - Async runtime
- `serde` / `serde_json` - JSON serialization
- `nix` - Unix socket peer credentials
- `shepherd-api` - Message types
- `shepherd-util` - Client IDs
