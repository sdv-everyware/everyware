//! The device's settings for the screen, as Google's HMI sample takes them
//! (system/software_defined_vehicle/samples/user_preferences/v1/hmi_service): the bundle finds
//! the owner of each settings group the screen shows, subscribes to the group at
//! user_preferences' UserPreferencesManagementService and serves its ChangeNotifier. The values
//! go to the screen as user_preferences reports them (IScreenLink.publishSettings, an
//! OnSettingsChangeRequest), and a change the screen asks for (ICommandSink.onSettingsChange, a
//! RequestSettingsChangeRequest) goes to user_preferences, which asks the owner to apply it. The
//! screen sees a new value only once the owner applied it and reported it.
//!
//! The messages are Google's, there is no proto of ours for settings:
//! system/software_defined_vehicle/automotive_services/user_preferences/vsidl/v1/
//! (change_notifier.proto, user_preferences_management_service.proto, setting.proto,
//! settings_group.proto). The groups and keys are the owner's: bundles/system/src/lib.rs.
//!
//! The changes are NON_PERSISTENT: the groups are SHARED, so the owner keeps the value and
//! reports it, and user_preferences keeps what the owner reports. A PERSISTENT change would be
//! stored per user, and this device has no users.

use crate::screen::Screen;
use crate::Last;
use async_trait::async_trait;
use com_android_sdv_user_preferences_user_preferences_management_service_rpc::Client as ManagementClient;
use com_android_sdv_user_preferences_view_change_notifier_rpc::Interface as ChangeNotifier;
use futures_util::stream::StreamExt;
use log::{debug, error, info, warn};
use protobuf::{Message, MessageField};
use sdv::comms::id::ServiceFqin;
use sdv::comms::sd::{ServiceDiscoveryManager, ServiceUnitChangeEvent, UnitType};
use crate::ContextRef;
use sdv::mw::{clientlib, Availability, Communicate};
use sdv::status::{SdvResult, SdvStatus, SdvStatusCode};
use sdv_mw_rs_com_everyware_dashboard_service_bundle::ClientDescriptors;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use user_preferences_api::change_notifier::{OnSettingsChangeRequest, OnSettingsChangeResponse};
use user_preferences_api::settings_group::SettingsGroupId;
use user_preferences_api::user_preferences_management_service::request_settings_change_request::ChangePersistencePolicy;
use user_preferences_api::user_preferences_management_service::{
    RequestSettingsChangeRequest, SubscribeToSettingsChangeAndGetSettingsRequest,
};

/// The settings groups the screen shows: the SDV package and the bundle that own each, and the
/// group's name (bundles/system).
const GROUPS: [(&str, &str, &str); 2] =
    [("com.everyware.system", "SystemServiceBundle", "time"), ("com.everyware.system", "SystemServiceBundle", "units")];
/// The only caller of the notifier: user_preferences (its sample APEX).
const USER_PREFERENCES_PACKAGE: &str = "com.android.sdv.sample.user_preferences";
const USER_PREFERENCES_BUNDLE: &str = "UserPreferencesServiceBundle";
/// How long a subscribe waits before it tries again: the owner registers its group after it
/// started, and user_preferences answers NotFound until then.
const RETRY: Duration = Duration::from_secs(5);

/// The ChangeNotifier user_preferences calls on every change of a followed group: into the
/// bundle's runtime.
pub struct Notifier {
    pub changes: UnboundedSender<OnSettingsChangeRequest>,
}

#[async_trait]
impl ChangeNotifier for Notifier {
    async fn OnSettingsChange(
        &self,
        caller_id: ServiceFqin,
        request: &OnSettingsChangeRequest,
    ) -> SdvResult<OnSettingsChangeResponse> {
        // The authz policy lets any client with the right declaration call; only
        // user_preferences reports settings.
        if caller_id.get_sdv_package_name() != USER_PREFERENCES_PACKAGE
            || caller_id.get_service_bundle_name() != USER_PREFERENCES_BUNDLE
        {
            warn!("settings change from {caller_id}: not user_preferences, dropped");
            return Err(SdvStatus::new(SdvStatusCode::PermissionDenied));
        }
        let _ = self.changes.send(request.clone());
        Ok(OnSettingsChangeResponse::new())
    }
}

/// The settings link: finds the groups' owners and user_preferences, subscribes to each group,
/// then passes the reported values to the screen and the screen's changes to user_preferences.
/// user_preferences keeps its subscribers in memory only: every time its server registers again
/// (it restarted), the bridge subscribes to every group again.
pub async fn settings_link(
    context: ContextRef,
    comms: Arc<dyn Communicate>,
    screen: Arc<Screen>,
    last: Arc<Mutex<Last>>,
    mut changes: UnboundedReceiver<OnSettingsChangeRequest>,
    mut requests: UnboundedReceiver<Vec<u8>>,
) {
    let descriptor =
        ClientDescriptors::<ManagementClient>::COM_ANDROID_SDV_USER_PREFERENCES_USER_PREFERENCES_MANAGEMENT_SERVICE;
    let mut registrations = match clientlib::create_server_registration_event_stream(comms.as_ref(), descriptor).await {
        Ok(events) => events,
        Err(e) => {
            error!("no settings for the screen: {e:?}");
            return;
        }
    };
    let (owners_tx, mut owners) = unbounded_channel();
    tokio::spawn(find_groups(context, owners_tx));
    let (subscribed_tx, mut subscribed) = unbounded_channel();
    // The groups whose owner is known, the ones subscribed at the current user_preferences, and
    // its client; a subscribe of an earlier one ends when the generation moves on.
    let mut groups: Vec<SettingsGroupId> = Vec::new();
    let mut followed: Vec<SettingsGroupId> = Vec::new();
    let mut management: Option<Arc<ManagementClient>> = None;
    let generation = Arc::new(AtomicU64::new(0));
    loop {
        tokio::select! {
            Some(event) = registrations.next() => {
                followed.clear();
                management = None;
                let now = generation.fetch_add(1, Ordering::SeqCst) + 1;
                if event != Availability::Available {
                    warn!("user_preferences is gone; the settings wait for it");
                    continue;
                }
                match clientlib::create_rpc_client(comms.as_ref(), descriptor, sdv::mw::ClientOptions::default()).await {
                    Ok(client) => {
                        let client = Arc::new(client);
                        info!("user_preferences is there: subscribing to {} settings groups", groups.len());
                        for group in &groups {
                            tokio::spawn(subscribe(client.clone(), group.clone(), subscribed_tx.clone(), generation.clone(), now));
                        }
                        management = Some(client);
                    }
                    Err(e) => error!("Failed to create the client of UserPreferencesManagementService: {e:?}"),
                }
            }
            Some(group) = owners.recv() => {
                if let Some(client) = &management {
                    let now = generation.load(Ordering::SeqCst);
                    tokio::spawn(subscribe(client.clone(), group.clone(), subscribed_tx.clone(), generation.clone(), now));
                }
                groups.push(group);
            }
            Some((started, settings)) = subscribed.recv() => {
                let settings: OnSettingsChangeRequest = settings;
                // an answer of a user_preferences that is gone since: its subscription is too
                if started != generation.load(Ordering::SeqCst) {
                    continue;
                }
                if let Some(group) = settings.settings_group_id.as_ref().cloned() {
                    if !followed.contains(&group) {
                        followed.push(group);
                    }
                    show(&screen, &last, settings, true);
                }
            }
            Some(change) = changes.recv() => {
                if followed.iter().any(|g| Some(g) == change.settings_group_id.as_ref()) {
                    show(&screen, &last, change, false);
                }
            }
            Some(bytes) = requests.recv() => match &management {
                Some(client) => request(client, &followed, &screen, &last, &bytes).await,
                None => warn!("a settings change while user_preferences is not there, dropped"),
            },
            else => break,
        }
    }
}

/// A group's values to the screen, the whole group kept for a panel that comes back. A
/// notification carries only the keys that changed (user_preferences' propagator): it is merged
/// into the kept group. A subscribe's answer is the whole group and replaces it.
fn show(screen: &Screen, last: &Mutex<Last>, settings: OnSettingsChangeRequest, whole: bool) {
    let Some(name) = settings.settings_group_id.as_ref().map(|g| g.name.clone()) else { return };
    let merged = {
        let mut last = last.lock().expect("last values");
        let kept = last.settings.entry(name.clone()).or_default();
        if whole {
            *kept = settings;
        } else {
            kept.settings_group_id = settings.settings_group_id;
            for setting in settings.active_settings {
                match kept.active_settings.iter_mut().find(|k| k.key == setting.key) {
                    Some(k) => *k = setting,
                    None => kept.active_settings.push(setting),
                }
            }
            kept.pending_changes = settings.pending_changes;
            kept.persisted_settings = settings.persisted_settings;
        }
        kept.clone()
    };
    info!("settings '{name}': {} values, {} pending", merged.active_settings.len(), merged.pending_changes.len());
    screen.publish_settings(&merged);
}

/// One change the screen asks for. Only for a followed group, and never persisted per user. A
/// change that fails sends the group's kept values again, so the screen drops what it showed.
async fn request(management: &ManagementClient, followed: &[SettingsGroupId], screen: &Screen, last: &Mutex<Last>, bytes: &[u8]) {
    let Ok(mut request) = RequestSettingsChangeRequest::parse_from_bytes(bytes) else {
        warn!("a settings change that does not parse");
        return;
    };
    let Some(group) = request.settings_group_id.as_ref().filter(|g| followed.contains(g)).cloned() else {
        warn!("a settings change of a group this bundle does not follow (yet), dropped");
        return;
    };
    request.change_persistence_policy = ChangePersistencePolicy::NON_PERSISTENT_CHANGE.into();
    let keys: Vec<&str> = request.settings.iter().map(|s| s.key.as_str()).collect();
    match management.RequestSettingsChange(&request).await {
        Ok(_) => info!("settings '{}': asked to change {keys:?}", group.name),
        Err(e) => {
            warn!("settings '{}': the change of {keys:?} failed: {e:?}", group.name);
            let kept = last.lock().expect("last values").settings.get(&group.name).cloned();
            if let Some(kept) = kept {
                screen.publish_settings(&kept);
            }
        }
    }
}

/// Subscribes to a group and sends its values, as a notification would carry them, to the link.
/// Tries again until it gets them (the owner may not have registered the group yet), as long as
/// user_preferences is the one it started with.
async fn subscribe(
    management: Arc<ManagementClient>,
    group: SettingsGroupId,
    subscribed: UnboundedSender<(u64, OnSettingsChangeRequest)>,
    generation: Arc<AtomicU64>,
    started: u64,
) {
    while generation.load(Ordering::SeqCst) == started {
        let request = SubscribeToSettingsChangeAndGetSettingsRequest {
            settings_group_id: MessageField::some(group.clone()),
            ..Default::default()
        };
        match management.SubscribeToSettingsChangeAndGetSettings(&request).await {
            Ok(response) => {
                info!("following the settings group '{}' version {}", group.name, response.version);
                let _ = subscribed.send((started, OnSettingsChangeRequest {
                    settings_group_id: MessageField::some(group.clone()),
                    active_settings: response.active_settings.iter().filter_map(|s| s.setting.as_ref().cloned()).collect(),
                    pending_changes: response.pending_settings.clone(),
                    persisted_settings: response.persisted_settings.clone(),
                    ..Default::default()
                }));
                return;
            }
            Err(e) => debug!("settings group '{}' not there yet ({e:?}); again in {} s", group.name, RETRY.as_secs()),
        }
        tokio::time::sleep(RETRY).await;
    }
}

/// The groups' ids, each sent once its owner is found (one search per owning bundle).
async fn find_groups(context: ContextRef, groups: UnboundedSender<SettingsGroupId>) {
    let mut owners: Vec<(&str, &str)> = GROUPS.iter().map(|(package, bundle, _)| (*package, *bundle)).collect();
    owners.dedup();
    for (package, bundle) in owners {
        let Some(owner) = find_owner(context, package, bundle).await else {
            error!("no owner {package}.{bundle} of a settings group");
            continue;
        };
        for (_, _, name) in GROUPS.iter().filter(|(p, b, _)| (*p, *b) == (package, bundle)) {
            let _ = groups.send(SettingsGroupId { service_fqin: owner.to_string(), name: name.to_string(), ..Default::default() });
        }
    }
}

/// The FQIN of the bundle that owns a group: the first instance of it that registers a
/// UserControllableService (as the HMI sample finds its HVAC service).
async fn find_owner(context: ContextRef, package: &str, bundle: &str) -> Option<ServiceFqin> {
    let unit_type = UnitType::builder()
        .set_sdv_package_name("com.android.sdv.user_preferences")
        .set_service_bundle_name("UserPreferencesServiceBundle")
        .set_unit_type_name("user_controllable.UserControllableService")
        .build()
        .ok()?;
    let sd = ServiceDiscoveryManager::new(context);
    let mut events = match sd.subscribe_service_unit_change_by_type(&unit_type).await {
        Ok(events) => events,
        Err(e) => {
            error!("cannot follow the UserControllableServices: {e:?}");
            return None;
        }
    };
    while let Some(event) = events.next().await {
        if let ServiceUnitChangeEvent::Registered(unit) = event {
            let identity = unit.get_service_bundle_identity();
            let fqin = identity.get_fqin();
            if fqin.get_sdv_package_name() == package && fqin.get_service_bundle_name() == bundle {
                return Some(fqin.clone());
            }
        }
    }
    None
}
