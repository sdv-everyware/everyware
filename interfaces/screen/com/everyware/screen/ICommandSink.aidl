package com.everyware.screen;

/**
 * The dashboard bundle's end of the screen's commands (IScreenLink.subscribeCommands). The bundle
 * calls the bundle that owns the device (SDV RPC) for each and hands the answer back with
 * IScreenLink.publishWifiResult or publishMatterResult.
 */
@VintfStability
interface ICommandSink {
    /** A com.everyware.WifiCommand (catalog/wifi.proto); a connect carries the password. */
    oneway void onWifiCommand(in byte[] command);

    /** A com.everyware.MatterCommand (catalog/matter_service.proto). */
    oneway void onMatterCommand(in byte[] command);
}
