# Smarthome additions to an SDV Media product. A board product inherits this file.

# Service bundles.
PRODUCT_PACKAGES += \
    com.everyware.weather \
    com.everyware.news \
    com.everyware.dashboard \
    com.everyware.wifi \
    com.everyware.matter \
    service_bundle_contexts

# The browser of the panel (docs/project/browser.md): the service, its script, the launcher of
# the Debian root, and the helper scripts and the bus configuration the root reads from the
# image. The root itself is not part of the image; without it the service ends at once.
PRODUCT_COPY_FILES += \
    vendor/sdv-everyware/everyware/product/everyware_browser.rc:$(TARGET_COPY_OUT_VENDOR)/etc/init/everyware_browser.rc \
    vendor/sdv-everyware/everyware/scripts/everyware-browser.sh:$(TARGET_COPY_OUT_VENDOR)/bin/everyware-browser.sh \
    vendor/sdv-everyware/everyware/scripts/glibc-enter.sh:$(TARGET_COPY_OUT_VENDOR)/bin/glibc-enter.sh \
    vendor/sdv-everyware/everyware/scripts/har-browser-cog.sh:$(TARGET_COPY_OUT_VENDOR)/etc/everyware/browser/har-browser-cog.sh \
    vendor/sdv-everyware/everyware/scripts/har-browser-relay.sh:$(TARGET_COPY_OUT_VENDOR)/etc/everyware/browser/har-browser-relay.sh \
    vendor/sdv-everyware/everyware/scripts/browser-bus.conf:$(TARGET_COPY_OUT_VENDOR)/etc/everyware/browser/browser-bus.conf
