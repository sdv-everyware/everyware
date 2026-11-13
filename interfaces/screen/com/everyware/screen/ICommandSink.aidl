package com.everyware.screen;

/**
 * The dashboard bundle's end of the screen's commands (IScreenLink.subscribeCommands). The bundle
 * calls the bundle that owns the device (SDV RPC) for each and hands the answer back with
 * IScreenLink.publishWifiResult or publishMatterResult. A settings change goes to user_preferences,
 * and the applied values come back with IScreenLink.publishSettings.
 */
@VintfStability
interface ICommandSink {
    /** A com.everyware.WifiCommand (catalog/wifi.proto); a connect carries the password. */
    oneway void onWifiCommand(in byte[] command);

    /** A com.everyware.MatterCommand (catalog/matter_service.proto). */
    oneway void onMatterCommand(in byte[] command);
    /**
     * A change of the device's settings: a com.android.sdv.user_preferences.RequestSettingsChangeRequest
     * (Google's user_preferences/vsidl/v1/user_preferences_management_service.proto) for a group
     * the screen got with IScreenLink.publishSettings. The bundle passes it to user_preferences;
     * the new values come back with publishSettings once the group's owner applied them.
     */
    oneway void onSettingsChange(in byte[] request);
}
