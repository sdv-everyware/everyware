# SDV EveryWare: documentation

SDV Media as the operating system of an embedded device, shown as a smart home panel on the
Qualcomm RB3 together with a second SDV Media device.

Read the concepts in order; each page has one idea, one diagram and a link to the next.

## Concepts: what and why

| | Page | Question it answers |
|---|------|---------------------|
| 1 | [The OS](concepts/1-the-os.md) | What is SDV Media, what is in it, what does it cost? |
| 2 | [Components](concepts/2-components.md) | What runs on a device and who starts whom? |
| 3 | [Service bundles](concepts/3-service-bundles.md) | What is a unit of function made of? |
| 4 | [Communication](concepts/4-communication.md) | How do bundles talk on one device, between devices, to the internet? |
| 5 | [UI](concepts/5-ui.md) | Why is the screen always a Figma design, and what can the code change? |
| 6 | [Security](concepts/6-security.md) | Who may do what? |
| 7 | [Extensions](concepts/7-extensions.md) | What do we add to SDV Media for a house, and why? |
| 8 | [UI components](concepts/8-ui-components.md) | How is a control drawn once and used everywhere, and what does the code change in it? |
| 9 | [Starting the app](concepts/9-starting-the-app.md) | How does the device's app start at boot, who restarts it, how is it developed? |
| 10 | [Updating the system](concepts/10-updating-the-system.md) | How are the OS and the service bundles updated, and what happens when an update fails? |

## Solution: how it is used in the house

- [Devices and dashboard](solution/devices-and-dashboard.md): devices, bundles, tile by tile.
- [AI agents](solution/ai-agents.md): MCP first, AppFunctions later, vsidl as the source.

## Project

- [Decisions](project/decisions.md): what was decided, when, why.
- [Status](project/status.md): what works, what is missing, next steps.
- [Open points](project/open-points.md): looked at, not decided yet (device discovery).
- [Glossary](glossary.md)

## Official documentation

- [Software defined vehicle: overview](https://source.android.com/docs/automotive/sdv), [Get started with the software defined vehicle](https://source.android.com/docs/automotive/sdv/getting-started)
- [Quick start: create and execute SDV service bundles](https://source.android.com/docs/automotive/sdv/workstreams/core/service-bundle-development), [VSIDL and middleware overview](https://source.android.com/docs/automotive/sdv/core-areas/vsidl)
- [Display Safety: the high availability renderer (HAR)](https://source.android.com/docs/automotive/sdv/display-safety): harry
- [Android Studio for Platform](https://developer.android.com/studio/platform)

Every concept page links the pages for its topic under "Official documentation".

## Guides and code

- The smarthome app and how to write a harry app, step by step:
  `packages/services/display_safety/utils/smarthome-app` (`docs/harry-app-anatomy.md`,
  `skills/harry-app/`).
- Adding a service bundle (catalog, vsidlc, APEX, deploy), as Android Studio for Platform
  does it: `skills/service-bundle/SKILL.md` (traps: `skills/service-bundle/traps.md`).
- The UI component library and how to add a component: `vendor/sdv-everyware/ui-lib`
  (`AGENTS.md`, `skills/ui-component/SKILL.md`).
- Scripts: `vendor/sdv-everyware/everyware/scripts/` (fetch the design, run the app on the RB3,
  deploy a bundle's APEX).

Older versions of these documents: `smarthome/backup/docs-2026-11-03/`.
