use bluer::Adapter;
use bluer::adv::{Advertisement, AdvertisementHandle, Type};
use tracing::{info, warn};

use crate::ancs::protocol;

pub struct Advertiser {
    adapter: Adapter,
    name: String,
    handle: Option<AdvertisementHandle>,
}

impl Advertiser {
    pub fn new(adapter: Adapter, name: String) -> Self {
        Self {
            adapter,
            name,
            handle: None,
        }
    }

    pub async fn arm(&mut self) {
        if self.handle.is_some() {
            return;
        }
        let advertisement = Advertisement {
            advertisement_type: Type::Peripheral,
            solicit_uuids: [protocol::SERVICE].into(),
            discoverable: Some(true),
            local_name: Some(self.name.clone()),
            ..Default::default()
        };
        match self.adapter.advertise(advertisement).await {
            Ok(handle) => {
                info!("soliciting ANCS as \"{}\"", self.name);
                self.handle = Some(handle);
            }
            Err(err) => warn!(
                "could not register the ANCS advertisement: {err} (try restarting bluetooth.service)"
            ),
        }
    }

    pub fn disarm(&mut self) {
        if self.handle.take().is_some() {
            info!("ANCS advertisement off air");
        }
    }

    pub async fn refresh(&mut self) {
        self.handle = None;
        self.arm().await;
    }
}
