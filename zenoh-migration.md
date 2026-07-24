# Zenoh migration — landed

This was the durable target spec for the MQTT→Zenoh transport migration, held
here while `CONTRACT.md` stayed MQTT-truthful. **The migration landed** — the
settled wire contract now lives in [`CONTRACT.md`](CONTRACT.md) (envelopes, key
scheme, discovery, the WS-JSON browser edge, the operator/claiming scoping
model), so this file is retired to a pointer rather than a second source of
truth. The decision, the source evaluation that chose Zenoh (the ROS 2 on-ramp,
query/reply as a first-class RPC primitive, brokerless peer discovery), and the
migration history are in [hub#9](https://github.com/sprocket-robotics/hub/issues/9).
