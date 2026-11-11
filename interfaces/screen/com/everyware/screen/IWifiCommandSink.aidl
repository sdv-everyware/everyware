package com.everyware.screen;

/** The dashboard bundle's end of the screen's Wi-Fi commands (IScreenLink.subscribeWifiCommands). */
@VintfStability
interface IWifiCommandSink {
    /** A com.everyware.WifiCommand (catalog/wifi.proto); it may carry a password. */
    oneway void onCommand(in byte[] command);
}
