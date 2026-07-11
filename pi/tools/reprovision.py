#!/usr/bin/env python3
"""Send a robot back to its re-onboarding window over the fabric:
`reprovision.py rover-b79c`. Publishes to robots/<id>/cmd/reprovision — the
robot reboots into a 3-minute window, then returns to operating mode on its
own.

TODO(hub#1): MQTT transport not yet implemented. hub-zenoh's version of this
script (Zenoh client, `pip install eclipse-zenoh`) is the reference shape —
port it to an MQTT publish once the client library is chosen.
"""

raise NotImplementedError("MQTT transport not yet implemented — see hub#1")
