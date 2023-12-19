use super::{
    behavior::{BehaviorScript, BehaviorScriptData},
    control::{Button, RelativeRotary},
    device::{Device, DevicePower},
    group::Group,
    light::Light,
    resource::{ResourceIdentifier, ResourceType},
    scene::{Scene, SceneBuilder},
    sensor::{
        CameraMotion, GeofenceClient, GeofenceClientBuilder, Geolocation, LightLevel, Motion,
        Temperature,
    },
    thirdparty::{HomeKit, Matter, MatterFabric},
    zigbee::{ZGPConnectivity, ZigbeeConnectivity, ZigbeeDeviceDiscovery},
    zone::{Home, Room, Zone, ZoneBuilder},
};
use crate::{
    api::{BridgeClient, HueAPIError, Version},
    command::CommandBuilder,
    event::HueEvent,
    ButtonData, Contact, ContactData, DeviceData, DevicePowerData, DeviceSoftwareUpdateData,
    GeofenceClientData, GeolocationData, GroupData, HomeData, HomeKitData, LightData,
    LightLevelData, MatterData, MatterFabricData, MotionData, RelativeRotaryData, Resource,
    SceneData, SmartScene, SmartSceneBuilder, SmartSceneData, TamperData, TemperatureData,
    ZGPConnectivityData, ZigbeeConnectivityData, ZigbeeDeviceDiscoveryData, ZoneData,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use std::{
    net::IpAddr,
    sync::{Mutex, MutexGuard},
    time::Duration,
};
use tokio::task::JoinHandle;

#[derive(Debug)]
pub enum BridgeDiscoveryError {
    NotFound,
    MDNSUnavailable,
}

#[derive(Debug)]
pub enum BridgeBuildError {
    NoIp,
    NoAppKey,
}

#[derive(Debug)]
pub enum BridgeUserError {
    UnableToCreate,
}

/// Core structure representing a Hue Bridge device interface.
#[derive(Debug)]
pub struct Bridge {
    pub(crate) api: Box<BridgeClient>,
    cache: Arc<Mutex<BridgeCache>>,
    poll_handle: Option<JoinHandle<()>>,
    #[cfg(feature = "sse")]
    listen_handle: Option<JoinHandle<()>>,
}

impl Bridge {
    pub fn new(addr: impl Into<IpAddr>, app_key: impl Into<String>) -> Self {
        let api = BridgeClient::new(addr.into(), app_key.into());
        Bridge {
            api: Box::new(api),
            cache: Arc::new(Mutex::new(BridgeCache::default())),
            poll_handle: None,
            #[cfg(feature = "sse")]
            listen_handle: None,
        }
    }

    fn from_api(api: BridgeClient) -> Self {
        Bridge {
            api: Box::new(api),
            cache: Arc::new(Mutex::new(BridgeCache::default())),
            poll_handle: None,
            #[cfg(feature = "sse")]
            listen_handle: None,
        }
    }

    pub async fn discover() -> Result<BridgeBuilder, BridgeDiscoveryError> {
        BridgeBuilder::discover().await
    }

    pub async fn poll(mut self, heartbeat: Duration) -> Self {
        let api = self.api.clone();
        let cache = self.cache.clone();

        if let Ok(data) = api.get_resources().await {
            insert_to_cache(&mut cache.lock().unwrap(), data)
        }

        self.poll_handle = Some(tokio::spawn(async move {
            let mut first_tick = true;
            let mut interval = tokio::time::interval(heartbeat);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

            loop {
                if first_tick {
                    first_tick = false;
                } else {
                    if let Ok(data) = api.get_resources().await {
                        insert_to_cache(&mut cache.lock().unwrap(), data)
                    }
                }
                interval.tick().await;
            }
        }));

        self
    }

    pub fn unpoll(&mut self) {
        if let Some(handle) = &self.poll_handle {
            handle.abort();
        }
        self.poll_handle = None;
    }

    #[cfg(feature = "sse")]
    pub async fn listen<C>(mut self, cb: C) -> Self
    where
        C: Fn(HashSet<ResourceIdentifier>) + Send + 'static,
    {
        let api = self.api.clone();
        let cache = self.cache.clone();

        if let Ok(data) = api.get_resources().await {
            insert_to_cache(&mut cache.lock().expect("lock cache"), data)
        }

        self.listen_handle = Some(tokio::spawn(async move {
            use futures_util::StreamExt;
            use reqwest_eventsource::Event;

            match api.get_event_stream().await {
                Ok(mut es) => {
                    while let Some(event) = es.next().await {
                        match event {
                            Ok(Event::Open) => {}
                            Ok(Event::Message(message)) => {
                                match serde_json::from_str::<Vec<HueEvent>>(&message.data) {
                                    Ok(data) => {
                                        let mut cache = cache.lock().expect("lock cache");
                                        let changes = upsert_to_cache(&mut cache, data);
                                        dbg!(&changes);
                                        cb(changes);
                                    }
                                    Err(e) => {
                                        dbg!(e);
                                    }
                                }
                            }
                            Err(e) => {
                                dbg!("Error: {}", e);
                            }
                        }
                    }
                }
                Err(e) => {
                    dbg!(e);
                }
            }
        }));

        self
    }

    #[cfg(feature = "sse")]
    pub fn unlisten(&mut self) {
        if let Some(handle) = &self.listen_handle {
            handle.abort();
        }
        self.listen_handle = None;
    }

    pub fn command(&self) -> CommandBuilder {
        CommandBuilder::new(&self)
    }

    pub async fn create_app(
        &mut self,
        app_name: impl Into<String>,
        instance_name: impl Into<String>,
    ) -> Result<&str, HueAPIError> {
        self.api.create_app(app_name, instance_name).await
    }

    #[deprecated = "only available via web interface with bridges running >=1.31.0"]
    pub async fn delete_app(&mut self, app_key: impl Into<String>) -> Result<(), HueAPIError> {
        self.api.delete_app(app_key).await
    }

    pub fn config(&self) -> Option<BridgeData> {
        self.cache
            .lock()
            .expect("lock cache")
            .data
            .as_ref()
            .map(|d| d.clone())
    }

    pub async fn refresh(&self) -> Result<(), HueAPIError> {
        let data = self.api.get_resources().await?;
        let mut cache = self.cache.lock().expect("lock cache");
        insert_to_cache(&mut cache, data);
        Ok(())
    }

    pub fn behavior_script(&self, id: impl Into<String>) -> Option<BehaviorScript> {
        self.cache
            .lock()
            .expect("lock cache")
            .behavior_scripts
            .get(&id.into())
            .map(|data| BehaviorScript::new(data.clone()))
    }

    pub fn behavior_scripts(&self) -> Vec<BehaviorScript> {
        self.cache
            .lock()
            .expect("lock cache")
            .behavior_scripts
            .iter()
            .map(|(_, data)| BehaviorScript::new(data.clone()))
            .collect()
    }

    pub fn button(&self, id: impl Into<String>) -> Option<Button> {
        self.cache
            .lock()
            .expect("lock cache")
            .buttons
            .get(&id.into())
            .map(|data| Button::new(data.clone()))
    }

    pub fn buttons(&self) -> Vec<Button> {
        self.cache
            .lock()
            .expect("lock cache")
            .buttons
            .iter()
            .map(|(_, data)| Button::new(data.clone()))
            .collect()
    }

    pub fn contact(&self, id: impl Into<String>) -> Option<Contact> {
        self.cache
            .lock()
            .expect("lock cache")
            .contacts
            .get(&id.into())
            .map(|data| Contact::new(&self, data.clone()))
    }

    pub fn contacts(&self) -> Vec<Contact> {
        self.cache
            .lock()
            .expect("lock cache")
            .contacts
            .iter()
            .map(|(_, data)| Contact::new(&self, data.clone()))
            .collect()
    }

    pub fn relative_rotary(&self, id: impl Into<String>) -> Option<RelativeRotary> {
        self.cache
            .lock()
            .expect("lock cache")
            .rotaries
            .get(&id.into())
            .map(|data| RelativeRotary::new(data.clone()))
    }

    pub fn relative_rotaries(&self) -> Vec<RelativeRotary> {
        self.cache
            .lock()
            .expect("lock cache")
            .rotaries
            .iter()
            .map(|(_, data)| RelativeRotary::new(data.clone()))
            .collect()
    }

    pub fn geolocation(&self, id: impl Into<String>) -> Option<Geolocation> {
        self.cache
            .lock()
            .expect("lock cache")
            .geolocations
            .get(&id.into())
            .map(|data| Geolocation::new(&self, data.clone()))
    }

    pub fn geolocations(&self) -> Vec<Geolocation> {
        self.cache
            .lock()
            .expect("lock cache")
            .geolocations
            .iter()
            .map(|(_, data)| Geolocation::new(&self, data.clone()))
            .collect()
    }

    pub fn geofence_client(&self, id: impl Into<String>) -> Option<GeofenceClient> {
        self.cache
            .lock()
            .expect("lock cache")
            .geofence_clients
            .get(&id.into())
            .map(|data| GeofenceClient::new(&self, data.clone()))
    }

    pub fn geofence_clients(&self) -> Vec<GeofenceClient> {
        self.cache
            .lock()
            .expect("lock cache")
            .geofence_clients
            .iter()
            .map(|(_, data)| GeofenceClient::new(&self, data.clone()))
            .collect()
    }

    pub async fn create_geofence_client(
        &self,
        builder: GeofenceClientBuilder,
    ) -> Result<GeofenceClient, HueAPIError> {
        let rid = self
            .api
            .post_geofence_client(serde_json::to_value(builder).unwrap())
            .await?;
        let data = self.api.get_geofence_client(rid.rid).await?;
        self.cache
            .lock()
            .expect("lock cache")
            .geofence_clients
            .insert(data.id.clone(), data.clone());
        Ok(GeofenceClient::new(&self, data))
    }

    pub async fn delete_geofence_client(
        &self,
        id: impl Into<String>,
    ) -> Result<Vec<ResourceIdentifier>, HueAPIError> {
        let res = self.api.delete_geofence_client(id).await?;
        delete_from_cache(&mut self.cache.lock().expect("lock cache"), &res);
        Ok(res)
    }

    pub async fn homekit(&self, id: impl Into<String>) -> Option<HomeKit> {
        self.cache
            .lock()
            .expect("lock cache")
            .homekits
            .get(&id.into())
            .map(|data| HomeKit::new(&self, data.clone()))
    }

    pub async fn homekits(&self) -> Vec<HomeKit> {
        self.cache
            .lock()
            .expect("lock cache")
            .homekits
            .iter()
            .map(|(_, data)| HomeKit::new(&self, data.clone()))
            .collect()
    }

    pub async fn matter(&self, id: impl Into<String>) -> Option<Matter> {
        self.cache
            .lock()
            .expect("lock cache")
            .matters
            .get(&id.into())
            .map(|data| Matter::new(&self, data.clone()))
    }

    pub async fn matters(&self) -> Vec<Matter> {
        self.cache
            .lock()
            .expect("lock cache")
            .matters
            .iter()
            .map(|(_, data)| Matter::new(&self, data.clone()))
            .collect()
    }

    pub async fn matter_fabric(&self, id: impl Into<String>) -> Option<MatterFabric> {
        self.cache
            .lock()
            .expect("lock cache")
            .matter_fabrics
            .get(&id.into())
            .map(|data| MatterFabric::new(data.clone()))
    }

    pub async fn matter_fabrics(&self) -> Vec<MatterFabric> {
        self.cache
            .lock()
            .expect("lock cache")
            .matter_fabrics
            .iter()
            .map(|(_, data)| MatterFabric::new(data.clone()))
            .collect()
    }

    pub async fn delete_matter_fabric(
        &mut self,
        id: impl Into<String>,
    ) -> Result<Vec<ResourceIdentifier>, HueAPIError> {
        let res = self.api.delete_matter_fabric(id).await?;
        delete_from_cache(&mut self.cache.lock().expect("lock cache"), &res);
        Ok(res)
    }

    pub async fn device(&self, id: impl Into<String>) -> Option<Device> {
        self.cache
            .lock()
            .expect("lock cache")
            .devices
            .get(&id.into())
            .map(|data| Device::new(&self, data.clone()))
    }

    pub fn devices(&self) -> Vec<Device> {
        self.cache
            .lock()
            .expect("lock cache")
            .devices
            .iter()
            .map(|(_, data)| Device::new(&self, data.clone()))
            .collect()
    }

    pub async fn delete_device(
        &mut self,
        id: impl Into<String>,
    ) -> Result<Vec<ResourceIdentifier>, HueAPIError> {
        let res = self.api.delete_device(id).await?;
        delete_from_cache(&mut self.cache.lock().expect("lock cache"), &res);
        Ok(res)
    }

    pub async fn device_power(&self, id: impl Into<String>) -> Option<DevicePower> {
        self.cache
            .lock()
            .expect("lock cache")
            .power
            .get(&id.into())
            .map(|data| DevicePower::new(data.clone()))
    }

    pub async fn device_powers(&self) -> Vec<DevicePower> {
        self.cache
            .lock()
            .expect("lock cache")
            .power
            .iter()
            .map(|(_, data)| DevicePower::new(data.clone()))
            .collect()
    }

    pub fn group(&self, id: impl Into<String>) -> Option<Group> {
        self.cache
            .lock()
            .expect("lock cache")
            .groups
            .get(&id.into())
            .map(|data| Group::new(&self, data.clone()))
    }

    pub fn groups(&self) -> Vec<Group> {
        self.cache
            .lock()
            .expect("lock cache")
            .groups
            .iter()
            .map(|(_, data)| Group::new(&self, data.clone()))
            .collect()
    }

    pub fn home(&self, id: impl Into<String>) -> Option<Home> {
        self.cache
            .lock()
            .expect("lock cache")
            .homes
            .get(&id.into())
            .map(|data| Home::new(data.clone()))
    }

    pub fn homes(&self) -> Vec<Home> {
        self.cache
            .lock()
            .expect("lock cache")
            .homes
            .iter()
            .map(|(_, data)| Home::new(data.clone()))
            .collect()
    }

    pub fn light(&self, id: impl Into<String>) -> Option<Light> {
        self.cache
            .lock()
            .expect("lock cache")
            .lights
            .get(&id.into())
            .map(|data| Light::new(&self, data.clone()))
    }

    pub fn lights(&self) -> Vec<Light> {
        self.cache
            .lock()
            .expect("lock cache")
            .lights
            .iter()
            .map(|(_, data)| Light::new(&self, data.clone()))
            .collect()
    }

    pub fn motion(&self, id: impl Into<String>) -> Option<Motion> {
        self.cache
            .lock()
            .expect("lock cache")
            .motions
            .get(&id.into())
            .map(|data| Motion::new(&self, data.clone()))
    }

    pub fn motions(&self) -> Vec<Motion> {
        self.cache
            .lock()
            .expect("lock cache")
            .motions
            .iter()
            .map(|(_, data)| Motion::new(&self, data.clone()))
            .collect()
    }

    pub fn motion_camera(&self, id: impl Into<String>) -> Option<CameraMotion> {
        self.cache
            .lock()
            .expect("lock cache")
            .motion_cameras
            .get(&id.into())
            .map(|data| CameraMotion::new(&self, data.clone()))
    }

    pub fn motion_cameras(&self) -> Vec<CameraMotion> {
        self.cache
            .lock()
            .expect("lock cache")
            .motion_cameras
            .iter()
            .map(|(_, data)| CameraMotion::new(&self, data.clone()))
            .collect()
    }

    pub fn room(&self, id: impl Into<String>) -> Option<Room> {
        self.cache
            .lock()
            .expect("lock cache")
            .rooms
            .get(&id.into())
            .map(|data| Room::new(&self, data.clone()))
    }

    pub fn rooms(&self) -> Vec<Room> {
        self.cache
            .lock()
            .expect("lock cache")
            .rooms
            .iter()
            .map(|(_, data)| Room::new(&self, data.clone()))
            .collect()
    }

    pub async fn create_room(&self, builder: ZoneBuilder) -> Result<Room, HueAPIError> {
        let rid = self
            .api
            .post_room(serde_json::to_value(builder).unwrap())
            .await?;
        let data = self.api.get_room(rid.rid).await?;
        self.cache
            .lock()
            .expect("lock cache")
            .rooms
            .insert(data.id.clone(), data.clone());
        Ok(Room::new(&self, data))
    }

    pub async fn delete_room(
        &self,
        id: impl Into<String>,
    ) -> Result<Vec<ResourceIdentifier>, HueAPIError> {
        let res = self.api.delete_room(id).await?;
        delete_from_cache(&mut self.cache.lock().expect("lock cache"), &res);
        Ok(res)
    }

    pub fn scene(&self, id: impl Into<String>) -> Option<Scene> {
        self.cache
            .lock()
            .expect("lock cache")
            .scenes
            .get(&id.into())
            .map(|data| Scene::new(&self, data.clone()))
    }

    pub fn scenes(&self) -> Vec<Scene> {
        self.cache
            .lock()
            .expect("lock cache")
            .scenes
            .iter()
            .map(|(_, data)| Scene::new(&self, data.clone()))
            .collect()
    }

    pub async fn create_scene(&self, builder: SceneBuilder) -> Result<Scene, HueAPIError> {
        let rid = self
            .api
            .post_scene(serde_json::to_value(builder).unwrap())
            .await?;
        let data = self.api.get_scene(rid.rid).await?;
        self.cache
            .lock()
            .expect("lock cache")
            .scenes
            .insert(data.id.clone(), data.clone());
        Ok(Scene::new(&self, data))
    }

    // // pub async fn update_scene(&mut self, )

    pub async fn delete_scene(
        &self,
        id: impl Into<String>,
    ) -> Result<Vec<ResourceIdentifier>, HueAPIError> {
        let res = self.api.delete_scene(id).await?;
        delete_from_cache(&mut self.cache.lock().expect("lock cache"), &res);
        Ok(res)
    }

    pub fn smart_scene(&self, id: impl Into<String>) -> Option<SmartScene> {
        self.cache
            .lock()
            .expect("lock cache")
            .smart_scenes
            .get(&id.into())
            .map(|data| SmartScene::new(&self, data.clone()))
    }

    pub fn smart_scenes(&self) -> Vec<SmartScene> {
        self.cache
            .lock()
            .expect("lock cache")
            .smart_scenes
            .iter()
            .map(|(_, data)| SmartScene::new(&self, data.clone()))
            .collect()
    }

    pub async fn create_smart_scene(
        &self,
        builder: SmartSceneBuilder,
    ) -> Result<SmartScene, HueAPIError> {
        let rid = self
            .api
            .post_smart_scene(serde_json::to_value(builder).unwrap())
            .await?;
        let data = self.api.get_smart_scene(rid.rid).await?;
        self.cache
            .lock()
            .expect("lock cache")
            .smart_scenes
            .insert(data.id.clone(), data.clone());
        Ok(SmartScene::new(&self, data))
    }

    // // pub async fn update_scene(&mut self, )

    pub async fn delete_smart_scene(
        &self,
        id: impl Into<String>,
    ) -> Result<Vec<ResourceIdentifier>, HueAPIError> {
        let res = self.api.delete_smart_scene(id).await?;
        delete_from_cache(&mut self.cache.lock().expect("lock cache"), &res);
        Ok(res)
    }

    pub async fn light_level(&self, id: impl Into<String>) -> Option<LightLevel> {
        self.cache
            .lock()
            .expect("lock cache")
            .light_levels
            .get(&id.into())
            .map(|data| LightLevel::new(&self, data.clone()))
    }

    pub async fn light_levels(&self) -> Vec<LightLevel> {
        self.cache
            .lock()
            .expect("lock cache")
            .light_levels
            .iter()
            .map(|(_, data)| LightLevel::new(&self, data.clone()))
            .collect()
    }

    pub async fn temperature(&self, id: impl Into<String>) -> Option<Temperature> {
        self.cache
            .lock()
            .expect("lock cache")
            .temps
            .get(&id.into())
            .map(|data| Temperature::new(&self, data.clone()))
    }

    pub async fn temperatures(&self) -> Vec<Temperature> {
        self.cache
            .lock()
            .expect("lock cache")
            .temps
            .iter()
            .map(|(_, data)| Temperature::new(&self, data.clone()))
            .collect()
    }

    pub async fn zgp_connectivity(&self, id: impl Into<String>) -> Option<ZGPConnectivity> {
        self.cache
            .lock()
            .expect("lock cache")
            .zgp_conns
            .get(&id.into())
            .map(|data| ZGPConnectivity::new(data.clone()))
    }

    pub async fn zgp_connectivities(&self) -> Vec<ZGPConnectivity> {
        self.cache
            .lock()
            .expect("lock cache")
            .zgp_conns
            .iter()
            .map(|(_, data)| ZGPConnectivity::new(data.clone()))
            .collect()
    }

    pub async fn zigbee_connectivity(&self, id: impl Into<String>) -> Option<ZigbeeConnectivity> {
        self.cache
            .lock()
            .expect("lock cache")
            .zigbee_conns
            .get(&id.into())
            .map(|data| ZigbeeConnectivity::new(&self, data.clone()))
    }

    pub async fn zigbee_connectivities(&self) -> Vec<ZigbeeConnectivity> {
        self.cache
            .lock()
            .expect("lock cache")
            .zigbee_conns
            .iter()
            .map(|(_, data)| ZigbeeConnectivity::new(&self, data.clone()))
            .collect()
    }

    pub async fn zigbee_device_discovery(
        &self,
        id: impl Into<String>,
    ) -> Option<ZigbeeDeviceDiscovery> {
        self.cache
            .lock()
            .expect("lock cache")
            .zigbee_dds
            .get(&id.into())
            .map(|data| ZigbeeDeviceDiscovery::new(&self, data.clone()))
    }

    pub async fn zigbee_device_discoveries(&self) -> Vec<ZigbeeDeviceDiscovery> {
        self.cache
            .lock()
            .expect("lock cache")
            .zigbee_dds
            .iter()
            .map(|(_, data)| ZigbeeDeviceDiscovery::new(&self, data.clone()))
            .collect()
    }

    pub async fn zzone(&self, id: impl Into<String>) -> Option<Zone> {
        self.cache
            .lock()
            .expect("lock cache")
            .zones
            .get(&id.into())
            .map(|data| Zone::new(&self, data.clone()))
    }

    pub async fn zones(&self) -> Vec<Zone> {
        self.cache
            .lock()
            .expect("lock cache")
            .zones
            .iter()
            .map(|(_, data)| Zone::new(&self, data.clone()))
            .collect()
    }

    pub async fn create_zone(&self, builder: ZoneBuilder) -> Result<Zone, HueAPIError> {
        let rid = self
            .api
            .post_zone(serde_json::to_value(builder).unwrap())
            .await?;
        let data = self.api.get_zone(rid.rid).await?;
        self.cache
            .lock()
            .expect("lock cache")
            .zones
            .insert(data.id.clone(), data.clone());
        Ok(Zone::new(&self, data))
    }

    pub async fn delete_zone(
        &self,
        id: impl Into<String>,
    ) -> Result<Vec<ResourceIdentifier>, HueAPIError> {
        let res = self.api.delete_zone(id).await?;
        delete_from_cache(&mut self.cache.lock().expect("lock cache"), &res);
        Ok(res)
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct BridgeData {
    /// Unique identifier representing a specific resource instance.
    pub id: String,
    /// Clip v1 resource identifier.
    pub id_v1: Option<String>,
    /// Owner of the service, in case the owner service is deleted, the service also gets deleted.
    pub owner: ResourceIdentifier,
    /// Unique identifier of the bridge as printed on the device. Lower case (shouldn't it be upper case?)
    pub bridge_id: String,
    pub time_zone: TimeZone,
}

#[derive(Clone, Debug, Deserialize)]
pub struct TimeZone {
    pub time_zone: String,
}

/// Builder structure representing a [Bridge] that is not yet fully configured.
pub struct BridgeBuilder {
    addr: Option<IpAddr>,
    app_key: Option<String>,
    version: Version,
}

impl Default for BridgeBuilder {
    fn default() -> Self {
        BridgeBuilder {
            addr: None,
            app_key: None,
            version: Default::default(),
        }
    }
}

impl BridgeBuilder {
    pub fn new() -> Self {
        BridgeBuilder::default()
    }

    async fn discover_http() -> Result<Self, BridgeDiscoveryError> {
        todo!()
    }

    #[cfg(feature = "mdns")]
    async fn discover_mdns() -> Result<Self, BridgeDiscoveryError> {
        use futures_util::{pin_mut, stream::StreamExt};
        const SERVICE_NAME: &'static str = "_hue._tcp.local";

        let stream = mdns::discover::all(SERVICE_NAME, Duration::from_secs(15))
            .unwrap()
            .listen();
        pin_mut!(stream);

        if let Some(Ok(response)) = stream.next().await {
            for rec in response.answers {
                match rec.kind {
                    mdns::RecordKind::A(addr) => {
                        return Ok(BridgeBuilder {
                            addr: Some(addr.into()),
                            ..Default::default()
                        })
                    }
                    _ => {}
                }
            }
            Err(BridgeDiscoveryError::NotFound)
        } else {
            Err(BridgeDiscoveryError::MDNSUnavailable)
        }
    }

    pub async fn discover() -> Result<Self, BridgeDiscoveryError> {
        #[cfg(feature = "mdns")]
        if let Ok(bridge) = BridgeBuilder::discover_mdns().await {
            return Ok(bridge);
        }
        BridgeBuilder::discover_http().await
    }

    pub fn app_key(mut self, key: &str) -> Self {
        self.app_key = Some(key.into());
        self
    }

    pub fn version(mut self, v: Version) -> Self {
        self.version = v;
        self
    }

    pub fn build(self) -> Bridge {
        let addr = self.addr.unwrap_or([0u8, 0, 0, 0].into());
        let app_key = self.app_key.unwrap_or_default();
        let api = if self.version == Version::V2 {
            BridgeClient::new(addr, app_key)
        } else {
            todo!()
        };

        Bridge::from_api(api)
    }
}

#[cfg(feature = "sse")]
fn upsert_to_cache(
    cache: &mut MutexGuard<'_, BridgeCache>,
    data: Vec<HueEvent>,
) -> HashSet<ResourceIdentifier> {
    use crate::event::{HueEventData, HueEventType};

    let mut changes: HashSet<ResourceIdentifier> = Default::default();

    for event in data {
        match event.etype {
            HueEventType::Update => {
                for event_data in event.data {
                    match event_data {
                        HueEventData::Button(patch) => {
                            let id = patch.get("id").expect("no id").as_str().unwrap().to_owned();
                            if let Some(data) = cache.buttons.get(&id) {
                                let data: ButtonData = merge_resource_data(data, patch);
                                changes.insert(data.rid());
                                cache.buttons.insert(id, data);
                            }
                        }
                        HueEventData::DevicePower(patch) => {
                            let id = patch.get("id").expect("no id").as_str().unwrap().to_owned();
                            if let Some(data) = cache.power.get(&id) {
                                let data: DevicePowerData = merge_resource_data(data, patch);
                                changes.insert(data.rid());
                                cache.power.insert(id, data);
                            }
                        }
                        HueEventData::Group(patch) => {
                            let id = patch.get("id").expect("no id").as_str().unwrap().to_owned();
                            if let Some(data) = cache.groups.get(&id) {
                                let data: GroupData = merge_resource_data(data, patch);
                                changes.insert(data.rid());
                                cache.groups.insert(id, data);
                            }
                        }
                        HueEventData::Light(patch) => {
                            let id = patch.get("id").expect("no id").as_str().unwrap().to_owned();
                            if let Some(data) = cache.lights.get(&id) {
                                let data: LightData = merge_resource_data(data, patch);
                                changes.insert(data.rid());
                                cache.lights.insert(id, data);
                            }
                        }
                        HueEventData::Scene(patch) => {
                            let id = patch.get("id").expect("no id").as_str().unwrap().to_owned();
                            if let Some(data) = cache.scenes.get(&id) {
                                let data: SceneData = merge_resource_data(data, patch);
                                changes.insert(data.rid());
                                cache.scenes.insert(id, data);
                            }
                        }
                        _ => {
                            dbg!("NEED TO DO THIS: {}", event_data);
                        }
                    }
                }
            }
            _ => {
                todo!()
            }
        }
    }

    changes
}

fn merge_resource_data<D: DeserializeOwned, S: Serialize>(data: S, patch: serde_json::Value) -> D {
    use json_patch::merge;
    let mut json = serde_json::to_value(data).unwrap();
    merge(&mut json, &patch);
    serde_json::from_value(json).unwrap()
}

#[derive(Debug, Default)]
pub(crate) struct BridgeCache {
    data: Option<BridgeData>,
    behavior_scripts: HashMap<String, BehaviorScriptData>,
    buttons: HashMap<String, ButtonData>,
    contacts: HashMap<String, ContactData>,
    devices: HashMap<String, DeviceData>,
    geofence_clients: HashMap<String, GeofenceClientData>,
    geolocations: HashMap<String, GeolocationData>,
    groups: HashMap<String, GroupData>,
    homes: HashMap<String, HomeData>,
    homekits: HashMap<String, HomeKitData>,
    lights: HashMap<String, LightData>,
    light_levels: HashMap<String, LightLevelData>,
    matters: HashMap<String, MatterData>,
    matter_fabrics: HashMap<String, MatterFabricData>,
    motions: HashMap<String, MotionData>,
    motion_cameras: HashMap<String, MotionData>,
    power: HashMap<String, DevicePowerData>,
    rooms: HashMap<String, ZoneData>,
    rotaries: HashMap<String, RelativeRotaryData>,
    scenes: HashMap<String, SceneData>,
    smart_scenes: HashMap<String, SmartSceneData>,
    swu: HashMap<String, DeviceSoftwareUpdateData>,
    tampers: HashMap<String, TamperData>,
    temps: HashMap<String, TemperatureData>,
    zigbee_conns: HashMap<String, ZigbeeConnectivityData>,
    zigbee_dds: HashMap<String, ZigbeeDeviceDiscoveryData>,
    zgp_conns: HashMap<String, ZGPConnectivityData>,
    zones: HashMap<String, ZoneData>,
}

fn insert_to_cache(cache: &mut MutexGuard<'_, BridgeCache>, data: Vec<Resource>) {
    for res in data {
        match res {
            // Resource::AuthV1 => {}
            Resource::BehaviorScript(d) => {
                cache.behavior_scripts.insert(d.id.clone(), d);
            }
            Resource::Bridge(d) => {
                cache.data = Some(d);
            }
            Resource::BridgeHome(d) => {
                cache.homes.insert(d.id.clone(), d);
            }
            Resource::Button(d) => {
                cache.buttons.insert(d.id.clone(), d);
            }
            Resource::CameraMotion(d) => {
                cache.motion_cameras.insert(d.id.clone(), d);
            }
            Resource::Contact(d) => {
                cache.contacts.insert(d.id.clone(), d);
            }
            Resource::Device(d) => {
                cache.devices.insert(d.id.clone(), d);
            }
            Resource::DevicePower(d) => {
                cache.power.insert(d.id.clone(), d);
            }
            Resource::DeviceSoftwareUpdate(d) => {
                cache.swu.insert(d.id.clone(), d);
            }
            Resource::GeofenceClient(d) => {
                cache.geofence_clients.insert(d.id.clone(), d);
            }
            Resource::Geolocation(d) => {
                cache.geolocations.insert(d.id.clone(), d);
            }
            Resource::Group(d) => {
                cache.groups.insert(d.id.clone(), d);
            }
            Resource::HomeKit(d) => {
                cache.homekits.insert(d.id.clone(), d);
            }
            Resource::Light(d) => {
                cache.lights.insert(d.id.clone(), d);
            }
            Resource::LightLevel(d) => {
                cache.light_levels.insert(d.id.clone(), d);
            }
            Resource::Matter(d) => {
                cache.matters.insert(d.id.clone(), d);
            }
            Resource::MatterFabric(d) => {
                cache.matter_fabrics.insert(d.id.clone(), d);
            }
            Resource::Motion(d) => {
                cache.motions.insert(d.id.clone(), d);
            }
            Resource::Room(d) => {
                cache.rooms.insert(d.id.clone(), d);
            }
            Resource::RelativeRotary(d) => {
                cache.rotaries.insert(d.id.clone(), d);
            }
            Resource::Scene(d) => {
                cache.scenes.insert(d.id.clone(), d);
            }
            Resource::SmartScene(d) => {
                cache.smart_scenes.insert(d.id.clone(), d);
            }
            Resource::Tamper(d) => {
                cache.tampers.insert(d.id.clone(), d);
            }
            Resource::Temperature(d) => {
                cache.temps.insert(d.id.clone(), d);
            }
            // Resource::ZigbeeBridgeConnectivity => {}
            Resource::ZigbeeConnectivity(d) => {
                cache.zigbee_conns.insert(d.id.clone(), d);
            }
            Resource::ZigbeeDeviceDiscovery(d) => {
                cache.zigbee_dds.insert(d.id.clone(), d);
            }
            Resource::ZGPConnectivity(d) => {
                cache.zgp_conns.insert(d.id.clone(), d);
            }
            Resource::Zone(d) => {
                cache.zones.insert(d.id.clone(), d);
            }
            Resource::Unknown => {
                dbg!("unknown {:?}", &res);
            }
            _ => {
                dbg!("unimplmented {:?}", &res);
            }
        }
    }
}

fn delete_from_cache(cache: &mut MutexGuard<'_, BridgeCache>, data: &Vec<ResourceIdentifier>) {
    let ids_by_type: HashMap<&ResourceType, HashSet<&String>> =
        data.into_iter().fold(Default::default(), |mut acc, r| {
            if !acc.contains_key(&r.rtype) {
                acc.insert(&r.rtype, Default::default());
            }
            acc.get_mut(&r.rtype).unwrap().insert(&r.rid);
            acc
        });
    for res in ids_by_type.keys() {
        let ids = ids_by_type.get(res).unwrap();
        match res {
            ResourceType::AuthV1 => {
                todo!()
            }
            ResourceType::BehaviorInstance => {
                todo!()
            }
            ResourceType::BehaviorScript => {
                cache.behavior_scripts.retain(|id, _| !ids.contains(&id));
            }
            ResourceType::Bridge => {
                // Is it possible to delete the bridge device?
                todo!()
            }
            ResourceType::BridgeHome => {
                cache.homes.retain(|id, _| !ids.contains(&id));
            }
            ResourceType::Button => {
                cache.buttons.retain(|id, _| !ids.contains(&id));
            }
            ResourceType::CameraMotion => {
                cache.motion_cameras.retain(|id, _| !ids.contains(&id));
            }
            ResourceType::Contact => {
                cache.contacts.retain(|id, _| !ids.contains(&id));
            }
            ResourceType::Device => {
                cache.devices.retain(|id, _| !ids.contains(&id));
            }
            ResourceType::DevicePower => {
                cache.power.retain(|id, _| !ids.contains(&id));
            }
            ResourceType::DeviceSoftwareUpdate => {
                cache.swu.retain(|id, _| !ids.contains(&id));
            }
            ResourceType::Entertainment => {
                todo!()
            }
            ResourceType::EntertainmentConfiguration => {
                todo!()
            }
            ResourceType::Geofence => {
                todo!()
            }
            ResourceType::GeofenceClient => {
                cache.geofence_clients.retain(|id, _| !ids.contains(&id));
            }
            ResourceType::Geolocation => {
                cache.geolocations.retain(|id, _| !ids.contains(&id));
            }
            ResourceType::Group => {
                cache.groups.retain(|id, _| !ids.contains(&id));
            }
            ResourceType::HomeKit => {
                cache.homekits.retain(|id, _| !ids.contains(&id));
            }
            ResourceType::Light => {
                cache.lights.retain(|id, _| !ids.contains(&id));
            }
            ResourceType::LightLevel => {
                cache.light_levels.retain(|id, _| !ids.contains(&id));
            }
            ResourceType::Matter => {
                cache.matters.retain(|id, _| !ids.contains(&id));
            }
            ResourceType::MatterFabric => {
                cache.matter_fabrics.retain(|id, _| !ids.contains(&id));
            }
            ResourceType::Motion => {
                cache.motions.retain(|id, _| !ids.contains(&id));
            }
            ResourceType::PublicImage => {
                todo!()
            }
            ResourceType::Recipe => {
                todo!()
            }
            ResourceType::RelativeRotary => {
                cache.rotaries.retain(|id, _| !ids.contains(&id));
            }
            ResourceType::Room => {
                cache.rooms.retain(|id, _| !ids.contains(&id));
            }
            ResourceType::Scene => {
                cache.scenes.retain(|id, _| !ids.contains(&id));
            }
            ResourceType::SmartScene => {
                cache.smart_scenes.retain(|id, _| !ids.contains(&id));
            }
            ResourceType::Tamper => {
                cache.tampers.retain(|id, _| !ids.contains(&id));
            }
            ResourceType::Taurus7455 => {
                todo!()
            }
            ResourceType::Temperature => {
                cache.temps.retain(|id, _| !ids.contains(&id));
            }
            ResourceType::ZGPConnectivity => {
                cache.zgp_conns.retain(|id, _| !ids.contains(&id));
            }
            ResourceType::ZigbeeBridgeConnectivity => {
                todo!()
            }
            ResourceType::ZigbeeConnectivity => {
                cache.zigbee_conns.retain(|id, _| !ids.contains(&id));
            }
            ResourceType::ZigbeeDeviceDiscovery => {
                cache.zigbee_dds.retain(|id, _| !ids.contains(&id));
            }
            ResourceType::Zone => {
                cache.zones.retain(|id, _| !ids.contains(&id));
            }
        }
    }
}
