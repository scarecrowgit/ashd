//! `org.bluez.Agent1`: BlueZ calls this object during pairing. Every request is forwarded
//! to the service loop as an [`AgentEv`] and answered from there (the UI shows a prompt and
//! the user's [`PairingReply`](crate::PairingReply) comes back through `reply`).

use std::time::Duration;

use async_channel::Sender;
use futures_lite::future::or;
use zbus::{interface, zvariant::OwnedObjectPath};

use crate::model::{PROMPT_SECS, PairingReply};

pub const AGENT_PATH: &str = "/org/ashd/bluez/agent";
pub const CAPABILITY: &str = "KeyboardDisplay";

/// What kind of request BlueZ made.
#[derive(Debug)]
pub enum AgentReq {
    RequestPinCode,
    DisplayPinCode { pin: String },
    RequestPasskey,
    DisplayPasskey { passkey: u32, entered: u16 },
    RequestConfirmation { passkey: u32 },
    RequestAuthorization,
    AuthorizeService { uuid: String },
}

#[derive(Debug)]
pub enum AgentEv {
    Request { device: String, req: AgentReq, reply: Sender<PairingReply> },
    /// BlueZ cancelled the outstanding request.
    Cancel,
}

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.bluez.Error")]
pub enum AgentError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Rejected(String),
    Canceled(String),
}

pub struct Agent {
    pub events: Sender<AgentEv>,
}

impl Agent {
    /// Forward a request and wait for the answer (or the timeout).
    async fn ask(&self, device: OwnedObjectPath, req: AgentReq) -> Result<PairingReply, AgentError> {
        let (reply, answer) = async_channel::bounded(1);
        self.events
            .send(AgentEv::Request { device: device.to_string(), req, reply })
            .await
            .map_err(|_| AgentError::Canceled("shell is gone".into()))?;
        let timeout = async {
            smol::Timer::after(Duration::from_secs(PROMPT_SECS)).await;
            Err(AgentError::Canceled("no answer".into()))
        };
        or(async { answer.recv().await.map_err(|_| AgentError::Canceled("cancelled".into())) }, timeout).await
    }

    async fn show(&self, device: OwnedObjectPath, req: AgentReq) {
        let (reply, _unused) = async_channel::bounded(1);
        let _ = self.events.send(AgentEv::Request { device: device.to_string(), req, reply }).await;
    }
}

#[interface(name = "org.bluez.Agent1")]
impl Agent {
    async fn release(&self) {
        let _ = self.events.send(AgentEv::Cancel).await;
    }

    async fn request_pin_code(&self, device: OwnedObjectPath) -> Result<String, AgentError> {
        match self.ask(device, AgentReq::RequestPinCode).await? {
            PairingReply::Pin(p) => Ok(p),
            _ => Err(AgentError::Rejected("rejected".into())),
        }
    }

    async fn display_pin_code(&self, device: OwnedObjectPath, pincode: String) {
        self.show(device, AgentReq::DisplayPinCode { pin: pincode }).await;
    }

    async fn request_passkey(&self, device: OwnedObjectPath) -> Result<u32, AgentError> {
        match self.ask(device, AgentReq::RequestPasskey).await? {
            PairingReply::Passkey(p) => Ok(p),
            _ => Err(AgentError::Rejected("rejected".into())),
        }
    }

    async fn display_passkey(&self, device: OwnedObjectPath, passkey: u32, entered: u16) {
        self.show(device, AgentReq::DisplayPasskey { passkey, entered }).await;
    }

    async fn request_confirmation(&self, device: OwnedObjectPath, passkey: u32) -> Result<(), AgentError> {
        match self.ask(device, AgentReq::RequestConfirmation { passkey }).await? {
            PairingReply::Accept => Ok(()),
            _ => Err(AgentError::Rejected("rejected".into())),
        }
    }

    async fn request_authorization(&self, device: OwnedObjectPath) -> Result<(), AgentError> {
        match self.ask(device, AgentReq::RequestAuthorization).await? {
            PairingReply::Accept => Ok(()),
            _ => Err(AgentError::Rejected("rejected".into())),
        }
    }

    async fn authorize_service(&self, device: OwnedObjectPath, uuid: String) -> Result<(), AgentError> {
        match self.ask(device, AgentReq::AuthorizeService { uuid }).await? {
            PairingReply::Accept => Ok(()),
            _ => Err(AgentError::Rejected("rejected".into())),
        }
    }

    async fn cancel(&self) {
        let _ = self.events.send(AgentEv::Cancel).await;
    }
}
