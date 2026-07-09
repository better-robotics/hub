# Login banner for the recovery console (serial autologin + ssh). Installed to
# /etc/profile.d/, so it runs on every interactive login and prints the one thing
# the operator needs after provisioning: the hub's IP — so rovers can be pointed
# at tcp/<ip>:7447 — plus whether the router is up. This is the *status* surface;
# Improv (the provisioning page) stays provisioning-only by design.
#
# Defensive on purpose: a profile.d script is sourced into the login shell, so it
# must never `exit`, never error out, and only act for interactive shells.

case $- in
  *i*)
    _hub_wlan_ip=$(ip -4 -o addr show wlan0 2>/dev/null | awk '{print $4}' | cut -d/ -f1 | head -n1)
    _hub_ssid=$(nmcli -t -f GENERAL.CONNECTION device show wlan0 2>/dev/null | cut -d: -f2)
    _hub_router=$(systemctl is-active hubd 2>/dev/null || echo unknown)

    printf '\n  %s — classroom Robotics Hub\n' "$(hostname 2>/dev/null || echo hub)"
    if [ -n "$_hub_wlan_ip" ]; then
      if [ -n "$_hub_ssid" ]; then
        printf '  Wi-Fi:    %s  (%s)\n' "$_hub_wlan_ip" "$_hub_ssid"
      else
        printf '  Wi-Fi:    %s\n' "$_hub_wlan_ip"
      fi
      printf '  Rovers:   tcp/%s:7447\n' "$_hub_wlan_ip"
    else
      printf '  Wi-Fi:    not connected — set it over Bluetooth (improv-wifi)\n'
    fi
    printf '  Recovery: ssh pi@10.55.0.1  (over this USB cable)\n'
    printf '  Router:   hubd %s   ·   logs: journalctl -u hubd -f\n\n' "$_hub_router"

    unset _hub_wlan_ip _hub_ssid _hub_router
    ;;
esac
