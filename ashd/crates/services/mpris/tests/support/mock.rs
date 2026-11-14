//! A mock MPRIS player served over a zbus connection (to a *private* bus). Shared by
//! `tests/bus.rs` and `examples/mock_player.rs` (`#[path]` include), so the panel can be
//! driven in the nested session by exactly what the tests exercise.
//!
//! The mock behaves like a real player: while `Playing` its `Position` advances with the
//! clock, `Next`/`Previous` move through the queue, `Seek`/`SetPosition` move the
//! position and emit `Seeked`, and every state change emits `PropertiesChanged`. Every
//! method call is appended to a log (`MockPlayer::log`) so tests can assert on commands.

#![allow(dead_code)]

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Instant,
};

use zbus::{
    Connection, fdo, interface,
    object_server::SignalEmitter,
    zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value},
};

pub const PATH: &str = "/org/mpris/MediaPlayer2";

#[derive(Clone, Debug)]
pub struct MockTrack {
    pub id: String,
    pub title: String,
    pub artists: Vec<String>,
    pub album: String,
    pub length_us: i64,
    pub art_url: Option<String>,
}

impl MockTrack {
    pub fn new(n: u32, title: &str, artist: &str, album: &str, secs: i64) -> Self {
        Self {
            id: format!("/org/mock/track/{n}"),
            title: title.into(),
            artists: if artist.is_empty() { vec![] } else { vec![artist.into()] },
            album: album.into(),
            length_us: secs * 1_000_000,
            art_url: None,
        }
    }
}

pub struct MockState {
    pub status: &'static str,
    pub tracks: Vec<MockTrack>,
    pub current: usize,
    base_us: i64,
    since: Instant,
    pub can_seek: bool,
    pub identity: String,
    pub desktop_entry: String,
    pub volume: f64,
    pub log: Vec<String>,
}

impl MockState {
    pub fn position_us(&self) -> i64 {
        let elapsed = if self.status == "Playing" { self.since.elapsed().as_micros() as i64 } else { 0 };
        let len = self.tracks.get(self.current).map_or(i64::MAX, |t| t.length_us);
        (self.base_us + elapsed).min(len)
    }

    fn rebase(&mut self, to: Option<i64>) {
        self.base_us = to.unwrap_or_else(|| self.position_us());
        self.since = Instant::now();
    }

    fn has_next(&self) -> bool {
        self.current + 1 < self.tracks.len()
    }

    fn has_prev(&self) -> bool {
        self.current > 0
    }

    fn track_md(t: &MockTrack) -> HashMap<String, OwnedValue> {
        let mut m: HashMap<String, OwnedValue> = HashMap::new();
        let mut put = |k: &str, v: Value<'_>| {
            m.insert(k.to_string(), v.try_to_owned().unwrap());
        };
        put("mpris:trackid", Value::from(ObjectPath::try_from(t.id.clone()).unwrap()));
        put("xesam:title", Value::from(t.title.clone()));
        put("xesam:artist", Value::from(t.artists.clone()));
        put("xesam:album", Value::from(t.album.clone()));
        put("mpris:length", Value::from(t.length_us));
        if let Some(a) = &t.art_url {
            put("mpris:artUrl", Value::from(a.clone()));
        }
        m
    }

    fn metadata(&self) -> HashMap<String, OwnedValue> {
        match self.tracks.get(self.current) {
            Some(t) => Self::track_md(t),
            None => {
                let mut m = HashMap::new();
                m.insert(
                    "mpris:trackid".to_string(),
                    Value::from(ObjectPath::try_from("/org/mpris/MediaPlayer2/TrackList/NoTrack").unwrap()).try_to_owned().unwrap(),
                );
                m
            }
        }
    }
}

pub type Shared = Arc<Mutex<MockState>>;

fn lock(s: &Shared) -> std::sync::MutexGuard<'_, MockState> {
    s.lock().unwrap_or_else(|e| e.into_inner())
}

/// `PropertiesChanged` for the Player interface (properties re-read by the service).
async fn emit_player(conn: &Connection) -> zbus::Result<()> {
    let r = conn.object_server().interface::<_, PlayerIface>(PATH).await?;
    let g = r.get().await;
    let em = r.signal_emitter();
    g.playback_status_changed(em).await?;
    g.metadata_changed(em).await?;
    g.can_go_next_changed(em).await?;
    g.can_go_previous_changed(em).await?;
    g.can_seek_changed(em).await?;
    Ok(())
}

struct Root(Shared);

#[interface(name = "org.mpris.MediaPlayer2")]
impl Root {
    fn raise(&self) {}
    fn quit(&self) {}
    #[zbus(property)]
    fn can_quit(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn can_raise(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn has_track_list(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn identity(&self) -> String {
        lock(&self.0).identity.clone()
    }
    #[zbus(property)]
    fn desktop_entry(&self) -> String {
        lock(&self.0).desktop_entry.clone()
    }
}

struct PlayerIface(Shared);

#[interface(name = "org.mpris.MediaPlayer2.Player")]
impl PlayerIface {
    async fn play_pause(&self, #[zbus(connection)] conn: &Connection) -> fdo::Result<()> {
        {
            let mut s = lock(&self.0);
            s.log.push("PlayPause".into());
            s.rebase(None);
            s.status = if s.status == "Playing" { "Paused" } else { "Playing" };
        }
        emit_player(conn).await?;
        Ok(())
    }
    async fn play(&self, #[zbus(connection)] conn: &Connection) -> fdo::Result<()> {
        {
            let mut s = lock(&self.0);
            s.log.push("Play".into());
            s.rebase(None);
            s.status = "Playing";
        }
        emit_player(conn).await?;
        Ok(())
    }
    async fn pause(&self, #[zbus(connection)] conn: &Connection) -> fdo::Result<()> {
        {
            let mut s = lock(&self.0);
            s.log.push("Pause".into());
            s.rebase(None);
            s.status = "Paused";
        }
        emit_player(conn).await?;
        Ok(())
    }
    async fn stop(&self, #[zbus(connection)] conn: &Connection) -> fdo::Result<()> {
        {
            let mut s = lock(&self.0);
            s.log.push("Stop".into());
            s.rebase(Some(0));
            s.status = "Stopped";
        }
        emit_player(conn).await?;
        Ok(())
    }
    async fn next(&self, #[zbus(connection)] conn: &Connection) -> fdo::Result<()> {
        {
            let mut s = lock(&self.0);
            s.log.push("Next".into());
            if s.has_next() {
                s.current += 1;
                s.rebase(Some(0));
            }
        }
        emit_player(conn).await?;
        Ok(())
    }
    async fn previous(&self, #[zbus(connection)] conn: &Connection) -> fdo::Result<()> {
        {
            let mut s = lock(&self.0);
            s.log.push("Previous".into());
            if s.has_prev() {
                s.current -= 1;
                s.rebase(Some(0));
            }
        }
        emit_player(conn).await?;
        Ok(())
    }
    async fn seek(&self, offset: i64, #[zbus(signal_emitter)] em: SignalEmitter<'_>) -> fdo::Result<()> {
        let pos = {
            let mut s = lock(&self.0);
            s.log.push(format!("Seek({offset})"));
            let to = (s.position_us() + offset).max(0);
            s.rebase(Some(to));
            to
        };
        Self::seeked(&em, pos).await?;
        Ok(())
    }
    async fn set_position(&self, track: ObjectPath<'_>, position: i64, #[zbus(signal_emitter)] em: SignalEmitter<'_>) -> fdo::Result<()> {
        let pos = {
            let mut s = lock(&self.0);
            s.log.push(format!("SetPosition({track},{position})"));
            let ok = s.tracks.get(s.current).is_some_and(|t| t.id == track.as_str());
            if ok {
                s.rebase(Some(position.max(0)));
            }
            ok.then_some(position.max(0))
        };
        if let Some(pos) = pos {
            Self::seeked(&em, pos).await?;
        }
        Ok(())
    }
    #[zbus(property)]
    fn playback_status(&self) -> String {
        lock(&self.0).status.to_string()
    }
    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedValue> {
        lock(&self.0).metadata()
    }
    #[zbus(property)]
    fn position(&self) -> i64 {
        lock(&self.0).position_us()
    }
    #[zbus(property)]
    fn volume(&self) -> f64 {
        lock(&self.0).volume
    }
    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        lock(&self.0).has_next()
    }
    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        lock(&self.0).has_prev()
    }
    #[zbus(property)]
    fn can_play(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_pause(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_seek(&self) -> bool {
        lock(&self.0).can_seek
    }
    #[zbus(property)]
    fn can_control(&self) -> bool {
        true
    }
    #[zbus(signal)]
    async fn seeked(em: &SignalEmitter<'_>, position: i64) -> zbus::Result<()>;
}

struct TrackListIface(Shared);

#[interface(name = "org.mpris.MediaPlayer2.TrackList")]
impl TrackListIface {
    fn get_tracks_metadata(&self, ids: Vec<ObjectPath<'_>>) -> Vec<HashMap<String, OwnedValue>> {
        let s = lock(&self.0);
        ids.iter().filter_map(|id| s.tracks.iter().find(|t| t.id == id.as_str())).map(MockState::track_md).collect()
    }
    async fn go_to(&self, id: ObjectPath<'_>, #[zbus(connection)] conn: &Connection) -> fdo::Result<()> {
        {
            let mut s = lock(&self.0);
            s.log.push(format!("GoTo({id})"));
            if let Some(i) = s.tracks.iter().position(|t| t.id == id.as_str()) {
                s.current = i;
                s.rebase(Some(0));
            }
        }
        emit_player(conn).await?;
        Ok(())
    }
    #[zbus(property)]
    fn tracks(&self) -> Vec<OwnedObjectPath> {
        lock(&self.0).tracks.iter().map(|t| OwnedObjectPath::try_from(t.id.clone()).unwrap()).collect()
    }
    #[zbus(property)]
    fn can_edit_tracks(&self) -> bool {
        false
    }
    #[zbus(signal)]
    async fn track_added(em: &SignalEmitter<'_>, metadata: HashMap<String, OwnedValue>, after: ObjectPath<'_>) -> zbus::Result<()>;
}

/// A running mock player.
pub struct MockPlayer {
    pub conn: Connection,
    pub shared: Shared,
    pub bus_name: String,
}

impl MockPlayer {
    /// Serve `org.mpris.MediaPlayer2.<short_name>` on the bus at `address`.
    pub async fn start(address: &str, short_name: &str, identity: &str, tracks: Vec<MockTrack>, with_tracklist: bool) -> zbus::Result<Self> {
        let shared: Shared = Arc::new(Mutex::new(MockState {
            status: "Stopped",
            tracks,
            current: 0,
            base_us: 0,
            since: Instant::now(),
            can_seek: true,
            identity: identity.into(),
            desktop_entry: short_name.into(),
            volume: 1.0,
            log: Vec::new(),
        }));
        let bus_name = format!("org.mpris.MediaPlayer2.{short_name}");
        let mut b = zbus::connection::Builder::address(address)?
            .serve_at(PATH, Root(shared.clone()))?
            .serve_at(PATH, PlayerIface(shared.clone()))?;
        if with_tracklist {
            b = b.serve_at(PATH, TrackListIface(shared.clone()))?;
        }
        let conn = b.name(bus_name.as_str())?.build().await?;
        Ok(Self { conn, shared, bus_name })
    }

    /// Every method call so far, e.g. `["PlayPause", "SetPosition(/org/mock/track/1,5000000)"]`.
    pub fn log(&self) -> Vec<String> {
        lock(&self.shared).log.clone()
    }

    pub fn position_us(&self) -> i64 {
        lock(&self.shared).position_us()
    }

    pub async fn set_status(&self, status: &'static str) -> zbus::Result<()> {
        {
            let mut s = lock(&self.shared);
            s.rebase(None);
            s.status = status;
        }
        emit_player(&self.conn).await
    }

    pub async fn set_can_seek(&self, can: bool) -> zbus::Result<()> {
        lock(&self.shared).can_seek = can;
        emit_player(&self.conn).await
    }

    pub async fn set_current(&self, index: usize) -> zbus::Result<()> {
        {
            let mut s = lock(&self.shared);
            s.current = index;
            s.rebase(Some(0));
        }
        emit_player(&self.conn).await
    }

    pub async fn set_art(&self, url: Option<String>) -> zbus::Result<()> {
        {
            let mut s = lock(&self.shared);
            let i = s.current;
            if let Some(t) = s.tracks.get_mut(i) {
                t.art_url = url;
            }
        }
        emit_player(&self.conn).await
    }

    /// The player jumps (as if the user seeked in its own UI): emits `Seeked`.
    pub async fn jump_to(&self, position_us: i64) -> zbus::Result<()> {
        lock(&self.shared).rebase(Some(position_us));
        let em = SignalEmitter::new(&self.conn, PATH)?;
        PlayerIface::seeked(&em, position_us).await
    }

    /// Append a track and emit `TrackAdded`.
    pub async fn add_track(&self, track: MockTrack) -> zbus::Result<()> {
        let (md, after) = {
            let mut s = lock(&self.shared);
            let after = s.tracks.last().map(|t| t.id.clone()).unwrap_or_else(|| "/org/mpris/MediaPlayer2/TrackList/NoTrack".into());
            let md = MockState::track_md(&track);
            s.tracks.push(track);
            (md, after)
        };
        let em = SignalEmitter::new(&self.conn, PATH)?;
        TrackListIface::track_added(&em, md, ObjectPath::try_from(after)?).await
    }
}
