# Smarthome additions to an SDV Media product. A board product inherits this file.

# Service bundles. The product also needs, from its board layer: the SELinux domains of the
# bundles in service_bundle_contexts (device/rb3/sdv_media_db845c/sepolicy), and SDV's
# user_preferences service for com.everyware.system (com.android.sdv.sample.user_preferences).
PRODUCT_PACKAGES += \
    com.everyware.weather \
    com.everyware.news \
    com.everyware.dashboard \
    com.everyware.wifi \
    com.everyware.matter \
    com.everyware.system \
    service_bundle_contexts

# The browser of the panel (docs/project/browser.md) is experimental and off by default: it
# runs WPE WebKit from a Debian root, entered as root, without WebKit's sandbox. Turn it on with
# EVERYWARE_EXPERIMENTAL_BROWSER=true (in the environment or the product): the image then gets the
# service, its script, the launcher of the Debian root, the helper scripts and the bus
# configuration the root reads, and ro.vendor.everyware.browser=true, by which the panel opens a
# story there. The root itself is not part of the image; without it the service ends at once.
# Design: https://github.com/sdv-everyware/everyware/issues/24; security:
# https://github.com/sdv-everyware/everyware/issues/54.
EVERYWARE_EXPERIMENTAL_BROWSER ?= false
ifeq ($(EVERYWARE_EXPERIMENTAL_BROWSER),true)
PRODUCT_VENDOR_PROPERTIES += ro.vendor.everyware.browser=true
PRODUCT_COPY_FILES += \
    vendor/sdv-everyware/everyware/product/everyware_browser.rc:$(TARGET_COPY_OUT_VENDOR)/etc/init/everyware_browser.rc \
    vendor/sdv-everyware/everyware/scripts/everyware-browser.sh:$(TARGET_COPY_OUT_VENDOR)/bin/everyware-browser.sh \
    vendor/sdv-everyware/everyware/scripts/glibc-enter.sh:$(TARGET_COPY_OUT_VENDOR)/bin/glibc-enter.sh \
    vendor/sdv-everyware/everyware/scripts/har-browser-cog.sh:$(TARGET_COPY_OUT_VENDOR)/etc/everyware/browser/har-browser-cog.sh \
    vendor/sdv-everyware/everyware/scripts/har-browser-relay.sh:$(TARGET_COPY_OUT_VENDOR)/etc/everyware/browser/har-browser-relay.sh \
    vendor/sdv-everyware/everyware/scripts/browser-bus.conf:$(TARGET_COPY_OUT_VENDOR)/etc/everyware/browser/browser-bus.conf
endif
