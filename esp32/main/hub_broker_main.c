/*
 * ESP32-as-hub feasibility test (better-robotics/hub-mqtt#2).
 *
 * Now the full local-hub slice: AP+STA+NAPT + Mosquitto broker + per-team
 * connect-auth, on one plain ESP32.
 *   - AP  (brobo-hub-test)  : students/rovers/laptop join here.
 *   - STA (venue Wi-Fi)     : uplink for internet.
 *   - NAPT                  : forwards AP-side traffic out the STA leg, so
 *                             joining the AP does NOT cut internet (the thing
 *                             that stranded the laptop in the AP-only test).
 *   - broker :1883          : starts unconditionally — the classroom works
 *                             locally even with no uplink; internet layers on
 *                             if/when the STA gets an IP.
 *
 * Browsers reach the broker via the WS bridge (ws_mqtt_bridge.c); rover/sim/
 * mosquitto_pub speak raw TCP directly.
 */
#include <string.h>
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "esp_wifi.h"
#include "esp_event.h"
#include "esp_log.h"
#include "esp_netif.h"
#include "nvs_flash.h"
#include "mosq_broker.h"
#include "mdns.h"

/* STA_SSID / STA_PASS — the venue uplink. Kept in a gitignored header so real
 * Wi-Fi credentials never land in committed source; copy wifi_creds.example.h
 * to wifi_creds.h and fill it in. */
#include "wifi_creds.h"

/* --- AP: what students/rovers/laptop join (demo creds, safe to commit) --- */
#define AP_SSID     "brobo-hub-test"
#define AP_PASS     "brobotics"        /* 8-63 chars → WPA2; "" → open */
#define AP_CHANNEL  1                  /* overridden to match STA channel in APSTA (single radio) */
#define AP_MAX_CONN 8                  /* esp32_nat_router's documented ceiling */

#define DHCPS_OFFER_DNS 0x02

/* ws_mqtt_bridge.c — lets browsers reach the broker over MQTT-over-WebSocket */
void start_ws_mqtt_bridge(void);

static const char *TAG = "hub-broker";
static esp_netif_t *ap_netif;
static esp_netif_t *sta_netif;

/* Per-team session auth. Whole-session accept/reject (no per-topic ACL); that's
 * sufficient under the per-team-broker topology (hub-mqtt#2). Mirrors
 * classroom.example.json5. */
static int connect_cb(const char *client_id, const char *username,
                      const char *password, int password_len)
{
    static const struct { const char *u, *p; } creds[] = {
        { "professor", "change-me" },
        { "team1",     "change-me-team1" },
        { "team2",     "change-me-team2" },
        { "rover",     "rover-secret" },
    };
    const char *cid = client_id ? client_id : "(none)";
    /* DEMO-ONLY: allow anonymous (no username) so the dashboard's credential-
     * free public fleet view works against this single broker (which has no
     * per-topic ACL). The real per-team model runs a broker PER team, so the
     * fleet view authenticates too — there is no anonymous tier there. */
    if (!username) {
        ESP_LOGI(TAG, "accept %s (anonymous, demo read tier)", cid);
        return 0;
    }
    if (!password) {
        ESP_LOGW(TAG, "reject %s: username with no password", cid);
        return 1;
    }
    for (size_t i = 0; i < sizeof(creds) / sizeof(creds[0]); i++) {
        if (strcmp(username, creds[i].u) == 0 && strcmp(password, creds[i].p) == 0) {
            ESP_LOGI(TAG, "accept %s as '%s'", cid, username);
            return 0;
        }
    }
    ESP_LOGW(TAG, "reject %s: bad credentials for '%s'", cid, username);
    return 1;
}

/* Hand AP clients a DNS server (the STA's), or they get an IP but can't
 * resolve names — the classic "connected, no internet". */
static void ap_offer_dns_from_sta(void)
{
    esp_netif_dns_info_t dns;
    if (esp_netif_get_dns_info(sta_netif, ESP_NETIF_DNS_MAIN, &dns) != ESP_OK) {
        return;
    }
    uint8_t offer = DHCPS_OFFER_DNS;
    esp_netif_dhcps_stop(ap_netif);
    esp_netif_dhcps_option(ap_netif, ESP_NETIF_OP_SET, ESP_NETIF_DOMAIN_NAME_SERVER,
                           &offer, sizeof(offer));
    esp_netif_set_dns_info(ap_netif, ESP_NETIF_DNS_MAIN, &dns);
    esp_netif_dhcps_start(ap_netif);
}

static void wifi_events(void *arg, esp_event_base_t base, int32_t id, void *data)
{
    if (base == WIFI_EVENT && id == WIFI_EVENT_STA_START) {
        esp_wifi_connect();
    } else if (base == WIFI_EVENT && id == WIFI_EVENT_STA_DISCONNECTED) {
        ESP_LOGW(TAG, "uplink down — retrying (AP + broker stay up regardless)");
        esp_wifi_connect();
    } else if (base == WIFI_EVENT && id == WIFI_EVENT_AP_STACONNECTED) {
        ESP_LOGI(TAG, "a device joined the AP");
    } else if (base == IP_EVENT && id == IP_EVENT_STA_GOT_IP) {
        ip_event_got_ip_t *e = (ip_event_got_ip_t *)data;
        ESP_LOGI(TAG, "uplink up, got IP " IPSTR " — enabling NAT + DNS for AP clients",
                 IP2STR(&e->ip_info.ip));
        ap_offer_dns_from_sta();
        esp_netif_set_default_netif(sta_netif);
        if (esp_netif_napt_enable(ap_netif) != ESP_OK) {
            ESP_LOGE(TAG, "NAPT enable failed");
        } else {
            ESP_LOGI(TAG, "NAT on: AP clients now route to the internet via the venue uplink");
        }
    }
}

void app_main(void)
{
    esp_err_t ret = nvs_flash_init();
    if (ret == ESP_ERR_NVS_NO_FREE_PAGES || ret == ESP_ERR_NVS_NEW_VERSION_FOUND) {
        ESP_ERROR_CHECK(nvs_flash_erase());
        ret = nvs_flash_init();
    }
    ESP_ERROR_CHECK(ret);

    ESP_ERROR_CHECK(esp_netif_init());
    ESP_ERROR_CHECK(esp_event_loop_create_default());
    ap_netif = esp_netif_create_default_wifi_ap();
    sta_netif = esp_netif_create_default_wifi_sta();

    ESP_ERROR_CHECK(esp_event_handler_instance_register(WIFI_EVENT, ESP_EVENT_ANY_ID,
                                                        &wifi_events, NULL, NULL));
    ESP_ERROR_CHECK(esp_event_handler_instance_register(IP_EVENT, IP_EVENT_STA_GOT_IP,
                                                        &wifi_events, NULL, NULL));

    wifi_init_config_t cfg = WIFI_INIT_CONFIG_DEFAULT();
    ESP_ERROR_CHECK(esp_wifi_init(&cfg));

    wifi_config_t ap = {
        .ap = {
            .ssid = AP_SSID, .ssid_len = strlen(AP_SSID), .channel = AP_CHANNEL,
            .password = AP_PASS, .max_connection = AP_MAX_CONN,
            .authmode = WIFI_AUTH_WPA2_PSK,
        },
    };
    if (strlen(AP_PASS) == 0) {
        ap.ap.authmode = WIFI_AUTH_OPEN;
    }
    wifi_config_t sta = {
        .sta = { .ssid = STA_SSID, .password = STA_PASS },
    };

    ESP_ERROR_CHECK(esp_wifi_set_mode(WIFI_MODE_APSTA));
    ESP_ERROR_CHECK(esp_wifi_set_config(WIFI_IF_AP, &ap));
    ESP_ERROR_CHECK(esp_wifi_set_config(WIFI_IF_STA, &sta));
    ESP_ERROR_CHECK(esp_wifi_start());
    ESP_LOGI(TAG, "APSTA up: AP '%s' (join this), STA → '%s' (uplink). "
                  "AP channel follows the venue's (single radio).", AP_SSID, STA_SSID);

    /* mDNS: advertise hostname "hub" so Apple/Bonjour clients reach the
     * dashboard at http://hub.local/ (matches the Pi's avahi name). Bare
     * "http://hub" is deliberately not attempted — Apple devices don't resolve
     * single-label names reliably; .local is the intended path. */
    if (mdns_init() == ESP_OK) {
        mdns_hostname_set("hub");
        mdns_instance_name_set("Better Robotics Hub");
        mdns_service_add(NULL, "_http", "_tcp", 80, NULL, 0);
        ESP_LOGI(TAG, "mDNS up: dashboard also at http://hub.local/");
    } else {
        ESP_LOGW(TAG, "mDNS init failed (http://hub.local won't resolve; IP still works)");
    }

    /* Bridge (httpd on :9001) starts first and returns; it dials the broker
     * lazily per browser, by which time the broker below is up. */
    start_ws_mqtt_bridge();

    /* Broker starts now, uplink or not — the classroom works offline. */
    struct mosq_broker_config bcfg = {
        .host = "0.0.0.0", .port = 1883, .tls_cfg = NULL, .handle_connect_cb = connect_cb,
    };
    ESP_LOGI(TAG, "starting broker on 0.0.0.0:1883 (raw MQTT; browsers reach it via the :9001 WS bridge)");
    mosq_broker_run(&bcfg); /* blocks */
}
