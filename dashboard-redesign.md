# Dashboard redesign — convergent direction

A design note for `dashboard.html`. Live state and decisions belong in the
tracking issue (**#4**, "Per-channel tiles on the dashboard"); this file is the
durable synthesis, not a status page.

## What this is

Several full redesign explorations of the classroom dashboard, done
independently, converged on the same architecture. That convergence is worth
recording because it *validates a direction the repo already adopted* (#4,
2026-07-10) rather than proposing a new one:

- **Devices/fleet as home.** A roster of device cards, each carrying capability
  chips derived from what the board publishes (`sys` today; `range`/`imu` as
  they land). A sensor station sits next to a robot as a peer — this is what
  makes the product "robotics + sensors," not "a remote control."
- **The channel is the unit of UI.** One channel → one tile on its board's card;
  the card layout mirrors the topic tree. Adopted standard, #4.
- **Every manual action shows its message.** The joystick and "the `pwm`
  messages it publishes" are the same thing under a rendered↔raw toggle. Driving
  by joystick and driving by a published message are revealed to be identical —
  the pub/sub mental model builds itself. This *is* #4's "rendered↔raw view
  toggle," reached independently.
- **Messages promoted from a debug drawer to a core concept.** The live wire log
  (`→ you send · ← the robot answers`) is the most educational surface in the
  product; it should read as a place students explore, not a troubleshooting
  closet. Per-topic message rates (e.g. `5/s`) are computable client-side from
  the stream already received — no broker introspection.
  **Landed 2026-07-16, and it took the Topics tab with it.** There is no drawer:
  Messages is a destination (`Fleet · Code · Messages`, Settings pinned below),
  holding the tree *and* the traffic — picking an address in the tree drives the
  log's filters, so "a topic is where a message goes" is the layout instead of an
  inference across two rooms. The tell that they were one destination all along
  was already in the file: the tree's arrows ARE the log's arrows, in the same
  tokens, with a comment in each surface keeping them honest.
  The rail badge is a GLOBAL msg/s, not the per-topic rates this bullet asks for
  — those are still owed. It exists for a different reason: the drawer was global
  chrome, so every panel could answer "is the wire alive?" without opening it, and
  moving the log into one destination would have cost that. The tree's per-topic
  counts are live; the rates are the same client-side arithmetic over `topicSeen`,
  unbuilt.
- **Missions as the spine, directing attention.** A step tracker turns free play
  into curriculum; the current step *emphasizes the relevant tile* (accent the
  line-sensor tile during "read the line sensor"), not just a progress bar.
  Strongest when steps are **predicates over observed wire traffic** — "read the
  distance sensor" completes when the hub sees that robot receive `range` — so
  progress is evidence, not a self-reported checkbox, and the operator's view
  comes free.
- **Operator = extra scope on the same UI**, not a separate app. Already how
  `myScope` gates the dashboard; the redesign just names the operator surface
  ("Fleet" / "Class") and adds the mission-progress summary tile.

## The one constraint every exploration broke: topic vocabulary

Each exploration invented a *different* friendly topic scheme (`robot/a044/cmd`,
`scout/sensors/distance`, `robots/a044/drive`). All are wrong. In a pub/sub
teaching tool the topic string **is** the API the student learns — they type it
into the publish box and into their own code minutes later. A display alias that
doesn't match the wire teaches a fiction that breaks their code.

Non-negotiable: UI strings come from `CONTRACT.md` verbatim.

- `robots/<id>/<channel>` — **plural `robots`**, load-bearing (the Mosquitto ACL
  and the firmware hardcode it). The friendly name ("Scout") lives in the card
  header; the topic path stays `robots/a044/…`.
- Channels: `pwm` (drive), `imu`, `set_led`; planned `range` (**not**
  "distance" — sensor-agnostic by design: HC-SR04 today, VL53L0X next kit),
  `cmd_vel`/`odom` for the encoder kit.
- Identity is in the topic, never the body. Renaming a robot must never rewrite
  its topics.

## What builds when — the timing gate

#4 already decided: **build the per-channel-tile framework when the third
channel lands** (`range`/`imu`, robot#4). Two channels don't need the
abstraction (generalize-too-early guard). So most of the explored surface is
correctly *not yet* — the sensor tiles and the rendered↔raw toggle arrive with
the sensor kit, not before.

The **topic tree shipped early** and this paragraph listed it as deferred until
2026-07-16. It didn't need the tile framework: the tree reads `topicSeen`, which
`logLine` already fills, so it cost a renderer and no abstraction. It now anchors
the Messages destination. The gate above still holds for everything keyed to a
*third channel* — the tree wasn't.

Safe and consistent to do before then (framing, not framework):

- Reframe the live-message surface so it reads as a concept, not a debug drawer.
  (Done, then superseded the same day by the merge above — it is a destination
  now, not a labelled drawer. Worth keeping for the failure it records: the
  label said "Live messages" until retiring the scope split (`16a2e9f`) deleted
  the line that set the title, and the markup's hardcoded "Log" won by default
  for three months — a debug word on the surface this bullet exists to protect,
  reintroduced as collateral from an auth refactor. A framing that lives in a
  branch can be deleted by someone editing the branch for another reason.)
- One-line cross-references that point between surfaces ("every stick move
  publishes to your robot's `pwm` topic — watch it in Messages"). Cheap,
  and it plants pub/sub before the explorer exists. Adds DOM → run the `verify`
  layout sweep (320/390/768/1200) before shipping.

## Where the pieces live (don't build in the wrong repo)

- **`dashboard.html`** — the fleet/drive/messages surface. Baked into the hubd
  binary via `include_str!`, runs on a LAN with no internet: **inline SVG only,
  no CDN assets** (the explorations all assumed a CDN icon font — that can't
  ship here).
- **`workbench`** — anything with a code editor. The Blocks↔Python view, and
  especially two-way sync ("edit either side"), is workbench's job, not the
  dashboard's. hubd already serves it at `/ide/`; the Code tab is a hand-off
  carrying robot identity, not an inline editor. The keeper idea for workbench:
  a **pub/sub-native block vocabulary** ("when message on topic…") over
  micro:bit-style event blocks.
- **Firmware** — "runs sandboxed on the hub" is not a system that exists; don't
  write UI copy that promises it.

## Open product decisions (these gate the tabs; not design calls)

1. **Student robot API shape.** How much does a friendly `drive(...)` hide
   `pwm`'s self-expiring watchdog (400 ms default, 4000 ms cap, enforced below
   every client)? A drive that isn't repeated stops on its own — an API that
   looks like a durable setpoint will make the first thing students build
   (drive-and-walk-away) behave mysteriously. Gates the Code tab.
2. **Missions: operator-authored or a fixed shipped set?** Gates the Missions
   tab and the authoring surface.

## Safety floor — carry it into every layout

An always-reachable **Stop** must survive any IA change (one exploration's
sidebar layout dropped it). The firmware watchdog already guarantees a drive
expires — the UI's job is to make the manual stop obvious, not to invent a new
safety model.
