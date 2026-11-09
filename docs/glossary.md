# Glossary

The official terms: [AAOS SDV terminology and glossary](https://source.android.com/docs/automotive/sdv/overview/terminology). Ours below are the
same words, kept to the house.

| Term | Meaning |
|------|---------|
| **Action** | in a harry app: "something happened" (a tap, a timer, a topic update); the only way the app's state changes |
| **Agent** | an SDV system service started by init: discovery, topics, RPC, lifecycle, ... |
| **APEX** | Android's updatable package for system components; service bundles are shipped in one |
| **authz** | SDV's authorization policies: which topics and services a bundle may use |
| **.dcf** | a fetched Figma design, the file harry draws from |
| **DICE** | Device Identifier Composition Engine: the cryptographic identity of a device |
| **DRM master** | the one process that may set what the display shows; on SDV Media, the harry app |
| **dt_agent** | the agent that carries topics |
| **GKI** | Generic Kernel Image, Android's common kernel |
| **harry** | HAR's renderer: draws a Figma design on the GPU, one process owns the display |
| **HAR** | High Availability Renderer, the screen part of SDV Media |
| **Layer name** | the `#name` of a Figma layer; the contract between design and code |
| **MCP** | Model Context Protocol: how an AI agent discovers and calls tools |
| **Mesh** | the `sdv_sd_agent`s of all devices, connected to each other |
| **Orchestration** | which instances of which bundles should run |
| **Peer** | another device in the mesh |
| **Presenter** | in a harry app: turns the state into values for the named layers |
| **RPC** | a call to a named service with an answer; carried by `rpcagent` |
| **SDV Core / SDV Media** | SDV without / with graphics, video, audio and HAR |
| **sdv_sd_agent** | service discovery: which services run where |
| **SELinux domain** | the set of things a process may touch; permissive logs, enforcing blocks |
| **Service bundle** | the unit of function: code + vsidl + orchestration + policy, one process |
| **State** | in a harry app: the app's data, changed only by actions |
| **Topic** | a named stream of values; publish once, any number of subscribers |
| **vsidl** | SDV's interface definition: messages, topics and RPC services of a bundle |
