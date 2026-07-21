#!/usr/bin/env python3
"""Send a robot back to its re-onboarding window over the fabric:
`reprovision.py robot-b79c`. Publishes to robots/<id>/cmd/reprovision — the
robot reboots into a 3-minute window, then returns to operating mode on its
own.

TODO(hub#1): not yet implemented as a standalone script. The mcp-bridge
(mcp-bridge/hub_mcp.py) already does this over the WS-JSON adapter — an operator
`{op:"pub"}` on robots/<id>/cmd/reprovision — so a standalone client speaks the
same adapter protocol (`websockets`). Kept as a stub until a sim-client suite
lands.
"""

raise NotImplementedError("not yet implemented — the mcp-bridge covers reprovision; see hub#1")
