# nosh

`nosh` is a roaming-tolerant remote shell built on QUIC — a successor to Mosh and
Eternal Terminal that reuses the user's existing SSH keys for mutual
authentication and runs over a single UDP/443 port (indistinguishable from
HTTP/3 on the wire). It targets developers who SSH from laptops and phones across
flaky, NAT'd, or firewalled networks and want sessions that survive IP changes
without re-authenticating.

This repository is currently at the **architecture-validation spike** (M0–M2,
Linux-only).

## Phase 1: QUIC transport skeleton

Phase 1 proves the foundational transport bet end-to-end: a single QUIC/UDP
connection (quinn + rustls, TLS 1.3, shared ALPN `nosh/0`) that carries a
reliable bidirectional stream **and** RFC 9221 datagrams concurrently, and stays
alive through interactive idle.

Workspace layout:

| Crate | Role |
|-------|------|
| `nosh-proto` | ALPN constant, `Message` enum, postcard codec, shared quinn transport config |
| `nosh-auth` | Placeholder TLS verifier — the seam Phase 2 fills with SSH-key cert-pinning + ssh-agent signing |
| `nosh-server` | QUIC echo server binary |
| `nosh-client` | QUIC client binary that drives the transport proofs |

> Auth (SSH keys), PTY/shell sessions, and roaming are **not** in Phase 1 — see
> `.planning/ROADMAP.md`.

## Run the demo

Two terminals. The server defaults to `127.0.0.1:4433` (unprivileged dev
default; UDP/443 is the production target).

**Terminal 1 — server:**

```sh
cargo run -p nosh-server
# or pick a port: cargo run -p nosh-server -- --port 14433
```

**Terminal 2 — client:**

```sh
cargo run -p nosh-client
# match the server's port if you changed it: cargo run -p nosh-client -- --port 14433
```

What you'll see:

- **Server** logs the accepted connection, the negotiated ALPN (`nosh/0`), and
  echoed streams/datagrams.
- **Client** logs, in order: `ALPN nosh/0 verified`, `stream echo matched`,
  `datagram round-trip matched` (with `max_datagram_size`), `concurrent stream +
  datagram round-trip ok`, `connection survived idle`, then exits 0.

## Run the tests

```sh
# Fast suite — handshake/ALPN, stream echo, datagram round-trip,
# stream+datagram coexistence, and a fast idle-survival proxy.
cargo test --workspace

# Includes the honest 60-second idle-survival test (TRANS-05).
cargo test --workspace -- --ignored
```
