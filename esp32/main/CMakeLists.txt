idf_component_register(SRCS "hub_broker_main.c" "ws_mqtt_bridge.c"
                       INCLUDE_DIRS "."
                       REQUIRES esp_wifi esp_netif nvs_flash esp_http_server lwip
                       EMBED_TXTFILES "../../dashboard.html")
