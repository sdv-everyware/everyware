package com.everyware.screen;

import com.everyware.screen.ICommandSink;

/**
 * The link between the screen and the house: the panel app serves this as
 * com.everyware.screen.IScreenLink/default, and the dashboard bundle is its only client. The
 * bundle pushes what the screen shows, and the screen's commands go back through the sink the
 * bundle hands in. The panel accepts a call only from the dashboard bundle (the SDV identity of
 * the calling process) and fails any other caller with EX_SECURITY.
 *
 * The values are the catalog's protobuf messages (catalog/*.proto) as bytes. After the panel
 * (re)started, the bundle sends the last value of each again.
 */
@VintfStability
interface IScreenLink {
    /** The weather of the house: a com.everyware.Weather (catalog/weather.proto). */
    void publishWeather(in byte[] weather);

    /** The news feed: a com.everyware.NewsFeed (catalog/news.proto). */
    void publishNews(in byte[] news);

    /** The thumbnail of a story of the feed: a com.everyware.NewsThumbnail (catalog/news.proto). */
    void publishNewsThumbnail(in byte[] thumbnail);

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

    /** The Matter devices of the house: a com.everyware.MatterDevices (catalog/matter.proto). */
    void publishMatterDevices(in byte[] devices);

    /**
     * The Matter bundle's answer to a command of the screen: which command ("pair", "on-off",
     * "level", "setpoint", "system-mode", "fan-mode"), the device it went to (0 for a pairing),
     * and a com.everyware.MatterResult (catalog/matter_service.proto).
     */
    void publishMatterResult(@utf8InCpp String command, long nodeId, int endpoint, in byte[] result);

    /**
     * The screen's commands go to this sink from now on. One sink at a time: while one is alive,
     * another fails with EX_ILLEGAL_STATE; the same sink again is taken. The panel watches the
     * sink: when its process dies, the screen shows that the house's data is gone until the next
     * subscribe.
     */
    void subscribeCommands(in ICommandSink sink);

    /**
     * A settings group of the device (after subscribeCommands: a new method goes last, so a
     * panel built before it keeps the numbers of the others), as user_preferences reports it: a
     * com.android.sdv.user_preferences.OnSettingsChangeRequest: the group's id and its active
     * values. The message is Google's, there is no proto of ours for it:
     * system/software_defined_vehicle/automotive_services/user_preferences/vsidl/v1/change_notifier.proto
     * (Setting in setting.proto, SettingsGroupId in settings_group.proto, same folder). The keys
     * and groups are the owner's: bundles/system/src/lib.rs (TIMEZONE, HOUR_CYCLE_24, TEMPERATURE).
     * Sent for each group the bundle follows, at subscribe and on every change.
     */
    void publishSettings(in byte[] settings);

    /**
     * The agenda of the house and the state of the calendar link: a com.everyware.Agenda
     * (catalog/calendar.proto). On every update and again after the panel (re)started. While no
     * account is linked it carries the setup page's URL for the QR code.
     */
    void publishAgenda(in byte[] agenda);
}
