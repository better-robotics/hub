#!/usr/bin/env python3
"""Test the MQTT-over-WebSocket path — the transport the browser dashboard
actually uses (Mosquitto's `listener 9001 protocol websockets`). The shell demo
(classroom-mosquitto-demo.sh) covers the ACL over raw TCP; this covers the same
model over WebSocket, so the path students depend on can't silently rot.

Self-contained: starts a throwaway Mosquitto with a WS listener + the repo ACL,
exercises it with paho-mqtt (transport="websockets"), tears down. Exits nonzero
on any failure (CI gate). Needs mosquitto + mosquitto_passwd + paho-mqtt.

  pip install paho-mqtt && python3 examples/ws-transport-test.py
"""
import os, shutil, subprocess, sys, tempfile, time
import paho.mqtt.client as mqtt

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
HOST, WS_PORT = "127.0.0.1", 19001  # non-default port: don't collide with a real broker


def newc(user, pw):
    c = mqtt.Client(mqtt.CallbackAPIVersion.VERSION2, transport="websockets")
    c.ws_set_options(path="/")           # mqtt.js connects with no path → "/"
    c.username_pw_set(user, pw)
    return c


def test_reject():
    """team1 with a wrong password → CONNACK not-authorised, through the WS listener."""
    st = {}
    c = newc("team1", "WRONG")
    c.on_connect = lambda cl, u, f, rc, p: st.__setitem__("rc", getattr(rc, "value", rc))
    c.connect(HOST, WS_PORT, 10)
    c.loop_start(); time.sleep(3); c.loop_stop()
    try: c.disconnect()
    except Exception: pass
    ok = st.get("rc") not in (0, None)
    print(f"  reject (wrong pw over WS): CONNACK rc={st.get('rc')} -> {'OK' if ok else 'FAIL'}")
    return ok


def test_roundtrip():
    """professor connects over WS, subscribes + publishes, receives its own message."""
    got = {}
    c = newc("professor", "change-me")
    def on_connect(cl, u, f, rc, p):
        got["conn"] = getattr(rc, "value", rc)
        if got["conn"] == 0:
            cl.subscribe("robots/team1/#")
    c.on_connect = on_connect
    c.on_subscribe = lambda cl, u, mid, rc, p: cl.publish("robots/team1/pwm", '{"left_motor":42}', qos=1)
    c.on_message = lambda cl, u, m: got.__setitem__("msg", (m.topic, m.payload.decode()))
    c.connect(HOST, WS_PORT, 10)
    c.loop_start()
    end = time.time() + 8
    while time.time() < end and "msg" not in got:
        time.sleep(0.1)
    c.loop_stop()
    try: c.disconnect()
    except Exception: pass
    ok = got.get("conn") == 0 and got.get("msg") is not None
    print(f"  roundtrip over WS: conn={got.get('conn')} received={got.get('msg')} -> {'OK' if ok else 'FAIL'}")
    return ok


def main():
    for tool in ("mosquitto", "mosquitto_passwd"):
        if not shutil.which(tool):
            print(f"FAIL: {tool} not on PATH (brew install mosquitto / apt-get install mosquitto)")
            return 1

    tmp = tempfile.mkdtemp()
    passwd, acl, conf = (os.path.join(tmp, n) for n in ("passwd", "acl.conf", "broker.conf"))
    subprocess.run(["mosquitto_passwd", "-b", "-c", passwd, "professor", "change-me"], check=True)
    subprocess.run(["mosquitto_passwd", "-b", passwd, "team1", "change-me-team1"], check=True)
    shutil.copy(os.path.join(REPO, "mosquitto-acl.example.conf"), acl)  # the real ACL, single-sourced
    os.chmod(passwd, 0o600); os.chmod(acl, 0o600)
    with open(conf, "w") as f:
        f.write(f"listener {WS_PORT}\nprotocol websockets\n"
                f"allow_anonymous true\npassword_file {passwd}\nacl_file {acl}\n")

    broker = subprocess.Popen(["mosquitto", "-c", conf],
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(1)
    try:
        print("== MQTT-over-WebSocket transport test (:%d) ==" % WS_PORT)
        ok = test_reject() & test_roundtrip()
    finally:
        broker.terminate(); broker.wait()
        shutil.rmtree(tmp, ignore_errors=True)
    print("WS_TRANSPORT_OK" if ok else "WS_TRANSPORT_FAIL")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
