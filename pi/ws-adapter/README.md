# ws-adapter — the browser edge of the Pi Zenoh hub

The Pi-side of the MQTT→Zenoh migration's browser tier (tracked in
[`#9`](https://github.com/sprocket-robotics/hub/issues/9); wire spec:
`../../zenoh-migration.md`). A browser can't speak native Zenoh, so the dashboard
speaks a small **WS-JSON op protocol** over one WebSocket and this process maps it
onto a zenoh session beside `zenohd`. It is the Python sibling of the ESP
firmware's `ws_zenoh_bridge.c` — same protocol, so **one dashboard serves both
tiers**:

| Client → adapter | Meaning |
|---|---|
| `{op:"sub", key}` / `{op:"unsub", key}` | declare/drop a per-client key filter |
| `{op:"pub", key, val}` | `session.put` (a `fleet/estop` write is gated on auth; a claimed robot's drive is gated on ownership) |
| `{op:"get", key, val, id}` → `{op:"reply", id, val}` | `session.get` (set_led, e-stop latch) |
| `{op:"auth", password}` → `{op:"auth", ok}` | the one operator gate |
| `{op:"hello", clientId}` → `{op:"owners", mine, held}` | bind an opaque browser identity; receive this client's ownership view (id lists, never tokens) |
| `{op:"claim", id}` / `{op:"release", id}` | claim/release a robot (claim needs a live BOOT-tap window) |
| adapter → client: `{key, val}` | a delivered subscription sample |
| adapter → client: `{op:"owner", id, state}` | an ownership change (`state` = `mine`/`held`/`free`), pushed per-recipient — never the owner's token |

The hub owns the `fleet/estop` latch: an authed estop pub updates it and a
queryable answers a (re)joining robot's join-time `get` — the retained MQTT
message, as a query.

## Run (beside `zenohd`)

```sh
pip install eclipse-zenoh websockets
ZENOH_CONNECT=tcp/127.0.0.1:7447 WS_PORT=9001 OPERATOR_PASS=<the classroom code> \
  python3 ws_zenoh_adapter.py
```

`ZENOH_CONNECT` points at the local `zenohd` (client mode). For a self-contained
bench (no router) set `ZENOH_LISTEN=tcp/127.0.0.1:7447` instead to run a peer with
its own listen endpoint. The dashboard connects to `ws://<host>:9001` — the same
`wsPort` convention the MQTT WS bridge used, so `dashboard.html` reaches this
unchanged.

## Authorization (deliberate: the Wi-Fi perimeter is the boundary)

The adapter reproduces the hub's Mosquitto ACL exactly (`pi/CLAUDE.md` §
Permissions; `../../CONTRACT.md` § Discovery & isolation): **everything under
`robots/**` and `pair/**` is open** to any client, authenticated or not — a
robot's name is a topic address, not a credential, so a per-topic gate would
protect nothing the hub's own Wi-Fi doesn't already. The **one** gated action is
engaging/clearing `fleet/estop`, which requires `{op:auth}` with the operator
code — deliberate friction so a stray tap can't halt or release the room. This is
the same posture as the ESP hub (`ws_zenoh_bridge.c`) and the broker it replaces;
gating `cmd/*` here would diverge from the contract, not harden it. `OPERATOR_PASS`
is a placeholder to rotate at deploy — an unset value warns loudly on startup
rather than silently admitting the public default.

## Per-owner claiming (hub#10 — opt-in exclusivity, not a new credential)

On top of that open floor, a student can **claim** a robot so nobody else drives
it. This is opt-in: an *unclaimed* robot stays open to everyone (the floor
above), and claiming adds exclusivity, not a password.

The claim is gated on **physical presence**, not a secret: a BOOT tap on the
robot opens a ~12 s window (it announces `robots/<id>/claimable`), during which
the adapter accepts **one** `{op:claim}`. So to claim robot X you must be
standing at X — no remote lockouts, and "stealing" a claimed robot means walking
over and pressing its button, which is self-policing in a classroom. Ownership
is keyed by an **opaque browser id** (`{op:hello, clientId}`, a random UUID from
localStorage), so a refresh keeps a student's robot; it carries no identity
beyond "same browser." The gate on `{op:"pub"}` drops non-zero drive to a
*claimed* robot from anyone but the owner or the operator — and a **stop
(zero-drive) always passes**, so isolation can never strand a robot in motion
(the robot's own safety floor and `fleet/estop` are untouched). The **operator
(`{op:auth}`) always overrides** and can `release` any robot.

That `clientId` is a **bearer token** — presenting it at `{op:hello}` is what
proves ownership at the gate — so it is treated as a secret: it never rides the
zenoh wire and is **never broadcast**. Each dashboard is told only whether a
robot is `mine`, `held`, or `free` (`{op:"owner", state}` per-recipient;
`{op:"owners", mine, held}` on join) — never *who* holds it. Broadcasting the raw
token would let any dashboard copy it off its own socket and impersonate the
owner; a per-recipient verdict can't be replayed, so a random UUID no one can see
can't be spoofed. Ownership lives only in the adapter, mirroring the ESP hub's
`ws_zenoh_bridge.c` byte-for-byte, so **one dashboard drives both tiers
identically**.

> **Pi tier — the adapter is made the sole drive path (hub#10 step 5).** On the
> Pi, `zenohd` *routes*, so a raw Zenoh client on the AP could reach a robot
> without passing through this adapter — unlike the ESP hub, where zenoh-pico's
> non-routing already makes the adapter the only path. The router ACL in
> `../zenoh-router.example.json5` restores that property: it denies writes to the
> command channels (`robots/*/pwm`, `robots/*/cmd/**`, `fleet/estop`) from AP-radio
> clients, so only the on-Pi adapter (over loopback) may inject them — and it
> applies per-owner + operator logic first. The **MCP bridge** rides this same
> edge (it speaks WS-JSON to this adapter as an operator, not raw Zenoh), so *all*
> drive flows through one place on both tiers. The ACL is staged with the router
> config; it takes effect when the migration cutover deploys `zenohd`.

## Validated (2026-07-21)

Browser-tested end-to-end against a real `dashboard.html` (the `zenohTransport`
swap) with a local zenoh peer + a test robot: the fleet card rendered from
telemetry, the e-stop banner armed from the latch query, operator sign-in
unlocked controls via `{op:auth}`, and an authed estop-clear + a joystick drive
both reached the robot. No Wi-Fi join — the browser hit `ws://localhost:9001`.
