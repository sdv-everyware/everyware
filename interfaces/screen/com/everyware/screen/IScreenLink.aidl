package com.everyware.screen;

import com.everyware.screen.IWifiCommandSink;

/**
 * The panel app serves this as com.everyware.screen.IScreenLink/default; the dashboard bundle is
 * its only client. The panel accepts a call only from the dashboard bundle (the SDV identity of
 * the calling process) and fails any other caller with EX_SECURITY.
 */
@VintfStability
interface IScreenLink {
    /** The Wi-Fi networks in range: a com.everyware.WifiNetworks (catalog/wifi.proto). */
    void publishWifiNetworks(in byte[] networks);

    /** The Wi-Fi link: a com.everyware.WifiState (catalog/wifi.proto). */
    void publishWifiState(in byte[] state);

    /**
     * The Wi-Fi bundle's answer to a command of the screen: which command ("scan", "connect",
     * "forget", "disconnect"), the SSID of a connect or forget, and a com.everyware.WifiResult
     * (catalog/wifi_service.proto).
     */
    void publishWifiResult(@utf8InCpp String command, @utf8InCpp String ssid, in byte[] result);

    /**
     * The screen's Wi-Fi commands go to this sink from now on. One sink at a time: while one is
     * alive, another fails with EX_ILLEGAL_STATE; the same sink again is taken. A sink whose process died is replaced by the next
     * subscribe, or dropped when a command fails on it.
     */
    void subscribeWifiCommands(in IWifiCommandSink sink);
}
