# The System bundle

`SystemServiceBundle` (APEX `com.everyware.system`) owns the system settings of the device:
the time zone and whether the clock shows 24 hours. The values live in SDV's
`user_preferences` service. The bundle registers them there as a user-controllable group,
applies a change and reports the value in force.

```
 a client (the settings sheet, through the dashboard bundle)
        | RequestSettingsChange / FactoryReset
        v
 user_preferences  (com.android.sdv.sample.user_preferences)
   stores the values, notifies subscribers
        | UserControllableService
        v
 SystemServiceBundle  (SELinux system_service_bundle)
   timezone       -> persist.sys.timezone   (bionic reads it)
   hour_cycle_24  -> kept in its own store (hour_cycle_24.pb)
        | UpdateSettings: the values in force
        v
 user_preferences
```

| Key | Type | Default | Applied by |
|---|---|---|---|
| `time/timezone` | text (UTF-8) | `Etc/UTC` | the system property `persist.sys.timezone` |
| `time/hour_cycle_24` | bool | true | the screen reads it, the bundle keeps it |

- A zone name is checked against the device's tzdata. An unknown name is refused, and the
  value in force is reported back.
- At start the bundle reports the values in force, because `user_preferences` does not push
  shared values at start.
- `FactoryReset` applies the defaults and reports them.
- If `user_preferences` is not there yet, the bundle retries the registration every 5 seconds.

## SELinux

Setting `persist.sys.timezone` needs `set_prop(timezone_prop)`, which the default bundle
domain has not. `product/service_bundle_contexts` maps the bundle to its own domain
`system_service_bundle` (device/rb3/sdv_media_db845c/sepolicy). It is the only rule the domain
adds to a service bundle.
