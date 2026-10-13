use std::collections::{HashMap, HashSet};
use std::time::Duration;

use anyhow::{Result, bail};
use bluer::{Adapter, AdapterEvent, AdapterProperty, Address, Device, DeviceEvent, DeviceProperty};
use futures::{Stream, StreamExt};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{interval, sleep, timeout};
use tracing::{debug, info, warn};

use crate::advert::Advertiser;
use crate::ancs::protocol;
use crate::ancs::session::{self, Characteristics, Options};
use crate::desktop::Desktop;

const ADVERT_REFRESH: Duration = Duration::from_secs(300);
const ANCS_POLL: Duration = Duration::from_secs(5);
const SESSION_RETRY: Duration = Duration::from_secs(3);
const CLASSIC_ONLY_GRACE: Duration = Duration::from_secs(30);

enum Link {
    Up(Address),
    Down(Address),
}

pub async fn run(adapter: Adapter, desktop: Desktop, name: String, options: Options) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut watchers: HashMap<Address, JoinHandle<()>> = HashMap::new();
    let mut live: HashSet<Address> = HashSet::new();
    let mut advertiser = Advertiser::new(adapter.clone(), name);

    let events = adapter.events().await?;
    tokio::pin!(events);

    let spawn = |address: Address, watchers: &mut HashMap<Address, JoinHandle<()>>| -> Result<()> {
        if watchers.contains_key(&address) {
            return Ok(());
        }
        let device = adapter.device(address)?;
        let task = tokio::spawn(watch(device, tx.clone(), desktop.clone(), options.clone()));
        watchers.insert(address, task);
        Ok(())
    };

    for address in adapter.device_addresses().await? {
        spawn(address, &mut watchers)?;
    }
    advertiser.arm().await;

    let mut refresh = interval(ADVERT_REFRESH);
    refresh.tick().await;

    loop {
        tokio::select! {
            event = events.next() => match event {
                Some(AdapterEvent::DeviceAdded(address)) => {
                    debug!("device {address} added");
                    spawn(address, &mut watchers)?;
                }
                Some(AdapterEvent::DeviceRemoved(address)) => {
                    info!("device {address} removed by BlueZ");
                    if let Some(task) = watchers.remove(&address) {
                        task.abort();
                    }
                    if live.remove(&address) && live.is_empty() {
                        advertiser.arm().await;
                    }
                }
                Some(AdapterEvent::PropertyChanged(AdapterProperty::Powered(true))) if live.is_empty() => {
                    advertiser.refresh().await;
                }
                Some(_) => {}
                None => bail!("adapter {} was removed", adapter.name()),
            },
            Some(link) = rx.recv() => match link {
                Link::Up(address) => {
                    live.insert(address);
                    advertiser.disarm();
                }
                Link::Down(address) => {
                    if live.remove(&address) && live.is_empty() {
                        advertiser.arm().await;
                    }
                }
            },
            _ = refresh.tick() => if live.is_empty() {
                advertiser.refresh().await;
            },
        }
    }
}

async fn watch(
    device: Device,
    tx: mpsc::UnboundedSender<Link>,
    desktop: Desktop,
    options: Options,
) {
    let address = device.address();
    if let Err(err) = watch_device(&device, &tx, &desktop, options).await {
        debug!("{address}: watcher stopped: {err}");
    }
}

async fn watch_device(
    device: &Device,
    tx: &mpsc::UnboundedSender<Link>,
    desktop: &Desktop,
    options: Options,
) -> Result<()> {
    let address = device.address();
    let events =
        device.events().await?.inspect(
            move |DeviceEvent::PropertyChanged(property)| match property {
                DeviceProperty::Connected(_)
                | DeviceProperty::Paired(_)
                | DeviceProperty::Trusted(_) => {
                    info!("{address}: {property:?}")
                }
                DeviceProperty::ServicesResolved(_) => debug!("{address}: {property:?}"),
                _ => {}
            },
        );
    tokio::pin!(events);
    let mut warned_classic_only = false;

    loop {
        let connected = device.is_connected().await?;
        if !connected {
            warned_classic_only = false;
        }
        let link_up = connected && device.is_services_resolved().await?;
        if link_up && let Some(chars) = find_ancs(device).await? {
            let origin = device.alias().await.unwrap_or_else(|_| address.to_string());
            info!("{origin} ({address}) offers ANCS");
            let mut notifications = desktop.for_device(origin);
            let up = || {
                let _ = tx.send(Link::Up(address));
            };

            let result = tokio::select! {
                result = session::run(&chars, &mut notifications, options.clone(), up) => result,
                () = link_lost(&mut events) => Ok(()),
            };
            match result {
                Ok(()) => info!("{address}: link lost"),
                Err(err) => warn!("{address}: ANCS session ended: {err}"),
            }

            notifications.release().await;
            let _ = tx.send(Link::Down(address));
            sleep(SESSION_RETRY).await;
            continue;
        }

        if link_up {
            if let Ok(None) = timeout(ANCS_POLL, events.next()).await {
                return Ok(());
            }
        } else if connected && !warned_classic_only {
            match timeout(CLASSIC_ONLY_GRACE, events.next()).await {
                Ok(None) => return Ok(()),
                Ok(Some(_)) => {}
                Err(_) => {
                    warned_classic_only = true;
                    warn_if_classic_only(device).await;
                }
            }
        } else if events.next().await.is_none() {
            return Ok(());
        }
    }
}

async fn warn_if_classic_only(device: &Device) {
    let has_ancs = device
        .uuids()
        .await
        .ok()
        .flatten()
        .is_some_and(|uuids| uuids.contains(&protocol::SERVICE));
    if has_ancs
        && device.is_paired().await.unwrap_or(false)
        && !device.is_services_resolved().await.unwrap_or(true)
    {
        let address = device.address();
        warn!(
            "{address} is connected over Classic Bluetooth only, with no LE link, so ANCS cannot run. \
             The bond probably has no LE keys: forget this desktop on the iPhone, run \
             `bluetoothctl remove {address}`, then pair again by tapping this desktop in the \
             iPhone's Bluetooth settings while ancs-linux is running"
        );
    }
}

async fn link_lost(events: &mut (impl Stream<Item = DeviceEvent> + Unpin)) {
    while let Some(DeviceEvent::PropertyChanged(property)) = events.next().await {
        if matches!(
            property,
            DeviceProperty::Connected(false) | DeviceProperty::ServicesResolved(false)
        ) {
            return;
        }
    }
}

async fn find_ancs(device: &Device) -> Result<Option<Characteristics>> {
    for service in device.services().await? {
        if service.uuid().await? != protocol::SERVICE {
            continue;
        }
        let (mut ns, mut cp, mut ds) = (None, None, None);
        for characteristic in service.characteristics().await? {
            match characteristic.uuid().await? {
                protocol::NOTIFICATION_SOURCE => ns = Some(characteristic),
                protocol::CONTROL_POINT => cp = Some(characteristic),
                protocol::DATA_SOURCE => ds = Some(characteristic),
                _ => {}
            }
        }
        return Ok(match (ns, cp, ds) {
            (Some(notification_source), Some(control_point), Some(data_source)) => {
                Some(Characteristics {
                    notification_source,
                    control_point,
                    data_source,
                })
            }
            _ => None,
        });
    }
    Ok(None)
}
