//! The libpulse side: a threaded mainloop whose callbacks only push [`Event`]s into a
//! channel; the service loop (async, on the `ashd-svc-audio` thread) owns the model and
//! calls back into [`Pulse`] (under the mainloop lock) to refresh and to execute actions.

use std::{cell::RefCell, rc::Rc};

use async_channel::Sender;
use libpulse_binding::{
    callbacks::ListResult,
    context::{
        Context, FlagSet, State,
        subscribe::InterestMaskSet,
    },
    mainloop::threaded::Mainloop,
    proplist::{Proplist, properties},
    volume::{ChannelVolumes, Volume},
};

use crate::{
    Device,
    model::{Action, Dir, NORM, RawDevice, kind_of, volume_of},
};

/// Messages from the pulse thread to the service loop.
pub(crate) enum Event {
    /// The context state changed; read it with [`Pulse::state`].
    StateChanged,
    /// A sink / source / server subscription event: refresh.
    Changed,
    Sinks(Vec<RawDevice>),
    Sources(Vec<RawDevice>),
    Defaults { sink: Option<String>, source: Option<String> },
}

pub(crate) struct Pulse {
    // Field order = drop order: the context must go before its mainloop.
    context: Context,
    mainloop: Mainloop,
    stopped: bool,
}

impl Pulse {
    /// Create the context and start connecting (never autospawns a daemon). The outcome
    /// arrives as [`Event::StateChanged`].
    pub fn connect(server: Option<&str>, tx: Sender<Event>) -> Result<Self, String> {
        let mut props = Proplist::new().ok_or("proplist")?;
        let _ = props.set_str(properties::APPLICATION_NAME, "ashd-shell");
        let _ = props.set_str(properties::APPLICATION_ID, "ashd.shell");
        let mut mainloop = Mainloop::new().ok_or("cannot create the pulse mainloop")?;
        let mut context = Context::new_with_proplist(&mainloop, "ashd-shell", &props).ok_or("cannot create the pulse context")?;

        let t = tx.clone();
        context.set_state_callback(Some(Box::new(move || {
            let _ = t.try_send(Event::StateChanged);
        })));
        let t = tx;
        context.set_subscribe_callback(Some(Box::new(move |_, _, _| {
            let _ = t.try_send(Event::Changed);
        })));

        mainloop.lock();
        let connected = context.connect(server, FlagSet::NOAUTOSPAWN, None);
        mainloop.unlock();
        connected.map_err(|e| format!("pa_context_connect: {e}"))?;
        mainloop.start().map_err(|e| format!("cannot start the pulse mainloop: {e}"))?;
        Ok(Self { context, mainloop, stopped: false })
    }

    pub fn state(&mut self) -> State {
        self.mainloop.lock();
        let s = self.context.get_state();
        self.mainloop.unlock();
        s
    }

    /// Subscribe to sink / source / server events (call once the context is ready).
    pub fn subscribe(&mut self) {
        self.mainloop.lock();
        let _op = self.context.subscribe(InterestMaskSet::SINK | InterestMaskSet::SOURCE | InterestMaskSet::SERVER, |_| {});
        self.mainloop.unlock();
    }

    /// Ask for the sink list, the source list and the defaults; answers arrive as events.
    pub fn refresh(&mut self, tx: &Sender<Event>) {
        self.mainloop.lock();
        let intro = self.context.introspect();

        let acc = Rc::new(RefCell::new(Vec::<RawDevice>::new()));
        let t = tx.clone();
        let _op = intro.get_sink_info_list(move |r| match r {
            ListResult::Item(i) => acc.borrow_mut().push(raw_sink(i)),
            ListResult::End => {
                let _ = t.try_send(Event::Sinks(std::mem::take(&mut *acc.borrow_mut())));
            }
            ListResult::Error => {}
        });

        let acc = Rc::new(RefCell::new(Vec::<RawDevice>::new()));
        let t = tx.clone();
        let _op = intro.get_source_info_list(move |r| match r {
            ListResult::Item(i) => {
                // Monitors of sinks are not inputs.
                if i.monitor_of_sink.is_none() {
                    acc.borrow_mut().push(raw_source(i));
                }
            }
            ListResult::End => {
                let _ = t.try_send(Event::Sources(std::mem::take(&mut *acc.borrow_mut())));
            }
            ListResult::Error => {}
        });

        let t = tx.clone();
        let _op = intro.get_server_info(move |i| {
            let _ = t.try_send(Event::Defaults {
                sink: i.default_sink_name.as_ref().map(|s| s.to_string()),
                source: i.default_source_name.as_ref().map(|s| s.to_string()),
            });
        });
        self.mainloop.unlock();
    }

    pub fn exec(&mut self, action: Action) {
        self.mainloop.lock();
        {
            let mut intro = self.context.introspect();
            match action {
                Action::Volume { dir, name, channels } => {
                    let mut cv = ChannelVolumes::default();
                    cv.set_len(channels.len().min(usize::from(ChannelVolumes::CHANNELS_MAX)) as u8);
                    for (slot, v) in cv.get_mut().iter_mut().zip(&channels) {
                        *slot = Volume(*v);
                    }
                    let _op = match dir {
                        Dir::Sink => intro.set_sink_volume_by_name(&name, &cv, None),
                        Dir::Source => intro.set_source_volume_by_name(&name, &cv, None),
                    };
                }
                Action::Mute { dir, name, muted } => {
                    let _op = match dir {
                        Dir::Sink => intro.set_sink_mute_by_name(&name, muted, None),
                        Dir::Source => intro.set_source_mute_by_name(&name, muted, None),
                    };
                }
                Action::MakeDefault { dir, name } => {
                    // (A `Context` method, not an introspector one.)
                    let _op = match dir {
                        Dir::Sink => self.context.set_default_sink(&name, |_| {}),
                        Dir::Source => self.context.set_default_source(&name, |_| {}),
                    };
                }
            }
        }
        self.mainloop.unlock();
    }

    fn shutdown(&mut self) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        self.mainloop.lock();
        self.context.set_state_callback(None);
        self.context.set_subscribe_callback(None);
        self.context.disconnect();
        self.mainloop.unlock();
        self.mainloop.stop();
    }
}

impl Drop for Pulse {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn raw_sink(i: &libpulse_binding::context::introspect::SinkInfo<'_>) -> RawDevice {
    let name = i.name.as_deref().unwrap_or_default().to_string();
    let port = i.active_port.as_ref();
    let channels: Vec<u32> = i.volume.get().iter().map(|v| v.0).collect();
    RawDevice {
        dev: Device {
            index: i.index,
            description: i.description.as_deref().unwrap_or(&name).to_string(),
            volume: volume_of(&channels),
            muted: i.mute,
            is_default: false,
            kind: kind_of(
                i.proplist.get_str("device.form_factor").as_deref(),
                i.proplist.get_str("device.bus").as_deref(),
                port.and_then(|p| p.name.as_deref()),
                &name,
                false,
            ),
            port: port.and_then(|p| p.description.as_deref()).map(str::to_string),
            name,
        },
        channels,
    }
}

fn raw_source(i: &libpulse_binding::context::introspect::SourceInfo<'_>) -> RawDevice {
    let name = i.name.as_deref().unwrap_or_default().to_string();
    let port = i.active_port.as_ref();
    let channels: Vec<u32> = i.volume.get().iter().map(|v| v.0).collect();
    RawDevice {
        dev: Device {
            index: i.index,
            description: i.description.as_deref().unwrap_or(&name).to_string(),
            volume: volume_of(&channels),
            muted: i.mute,
            is_default: false,
            kind: kind_of(None, None, None, &name, true),
            port: port.and_then(|p| p.description.as_deref()).map(str::to_string),
            name,
        },
        channels,
    }
}

const _: () = assert!(NORM == 0x10000);
