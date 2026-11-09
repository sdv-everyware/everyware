# matter-generator

A virtual Matter device for SDV EveryWare: one Matter node with the four devices of the
dashboard, so that the panel's Matter bundle has a real device to commission, read and drive.
It runs on a Linux host in the same network as the panel. It is built on
[rs-matter](https://github.com/project-chip/rs-matter), the library the bundle uses too,
pinned to one commit in `Cargo.toml` (its API still moves).

## The node

| Endpoint | Device type | Clusters | Dashboard tile |
|---|---|---|---|
| 0 | Root Node | Basic Information, General Diagnostics, the commissioning and security clusters | |
| 1 | Thermostat | Descriptor, Identify, Thermostat, Relative Humidity Measurement | Home climate |
| 2 | On/Off Light | Descriptor, Identify, Groups, On/Off | Entryway |
| 3 | Dimmable Light | Descriptor, Identify, Groups, On/Off, Level Control | Bedroom |
| 4 | Fan | Descriptor, Identify, Groups, Fan Control | Office |

What each device serves:

| Device | Attributes | Commands | At the first start |
|---|---|---|---|
| Basic Information | VendorName "EveryWare", ProductName "Virtual Home", SerialNumber "EW-VH-0001", SoftwareVersionString "0.1.0", the test vendor and product IDs | | |
| General Diagnostics | UpTime, BootReason and the rest rs-matter serves | | |
| Thermostat (heat, cool, auto) | LocalTemperature, Occupied Heating and Cooling Setpoint, SystemMode, ThermostatRunningState, ThermostatRunningMode, the four setpoint limits of each, MinSetpointDeadBand, ControlSequenceOfOperation (cooling and heating) | SetpointRaiseLower; writes to the setpoints and SystemMode | heat, 22 °C heating, 26 °C cooling, the room at 20.5 °C |
| Relative Humidity Measurement | MeasuredValue, MinMeasuredValue (40 %), MaxMeasuredValue (55 %) | | 45 % |
| On/Off (entryway, bedroom) | OnOff, StartUpOnOff (null: back as it was after a power loss), OnTime, OffWaitTime, GlobalSceneControl | Off, On, Toggle, OffWithEffect, OnWithRecallGlobalScene, OnWithTimedOff | on |
| Level Control (bedroom) | CurrentLevel, Min and MaxLevel (1, 254), OnOffTransitionTime (0.5 s), OnLevel, On and OffTransitionTime, DefaultMoveRate, Options, StartUpCurrentLevel, RemainingTime | MoveToLevel, Move, Step, Stop and their WithOnOff forms | 166 (65 %) |
| Fan Control (office) | FanMode, FanModeSequence (Off, Low, Medium, High), PercentSetting, PercentCurrent, RockSupport and RockSetting (left-right), WindSupport and WindSetting (natural wind), AirflowDirection | writes to FanMode, PercentSetting, RockSetting, WindSetting, AirflowDirection | Low (33 %) |

Not served: Wi-Fi Network Diagnostics (a host has no Matter Wi-Fi driver to report RSSI or
BSSID; the root endpoint is the Ethernet one).

## The simulation

The room behind the thermostat moves every 5 s (`src/sim.rs`): 0.2 °C per tick while heating
or cooling, 0.05 °C back to 19 °C while idle. A relay closes 0.3 °C past its setpoint and opens
when the setpoint is reached. The humidity wanders by up to 0.2 % per tick within 40 to 55 %,
and drops a little while cooling. A change is reported to the subscribers. Commands from a
controller change the state and are logged ("bedroom: level 200 (79 %)", "office: fan
Manual { .. }").

The fabrics, the access control and the clusters' settings are kept in the storage directory:
a restart keeps the node commissioned and its lights, level and fan as they were.

## Build and run

It needs IPv6 on the host (Matter runs over IPv6; without it the start fails with
"Address family not supported by protocol").

```bash
# WHAT: build the generator with the tree's Rust toolchain
# WHY: no system Rust is needed; rs-matter and its crates come from GitHub and crates.io
cd vendor/sdv-everyware/everyware/tools/matter-generator
PATH=$ANDROID_BUILD_TOP/prebuilts/rust-toolchain/linux-x86/1.95.0/bin:$PATH cargo build --release

# WHAT: run the node; prints the QR code and the pairing code while not commissioned
# WHY: the panel's Matter bundle commissions it with the pairing code
./target/release/matter-generator --storage ~/.local/state/matter-generator
```

| Option | Default | What |
|---|---|---|
| `--discriminator N` | 3840 | the 12-bit discriminator of the commissioning advertisement |
| `--passcode N` | 20202021 | the setup passcode (the pairing code is made from it) |
| `--port N` | 5540 | the Matter UDP port |
| `--storage DIR` | `<tmp>/matter-generator` | the node's state; delete it to commission the node again |

The defaults are the test values chip-tool uses: `chip-tool pairing onnetwork 1 20202021`
commissions it, `chip-tool onoff toggle 1 2` toggles the entryway light.

## Tests

```bash
# WHAT: the simulation's unit tests
cargo test
```
