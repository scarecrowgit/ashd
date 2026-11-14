//! The pure core: players keyed by bus name, update merging, and the choice of the
//! "active" player. No D-Bus here; the service feeds it events.
//!
//! Every event that carries an `owner` (the unique bus name the watcher was started for)
//! is ignored when the player has since changed owner: a stale watcher cannot overwrite
//! the fresh one's data.

use std::{collections::BTreeMap, time::Duration};

use crate::{
    MprisState, PlaybackStatus, Player, Track,
    parse::{PlayerProps, RootProps},
};

struct Entry {
    player: Player,
    /// Unique bus name of the connection currently owning `player.bus_name`.
    owner: String,
    /// Everything of the last `GetAll(Player)` except the position (change detection).
    last: PlayerProps,
    /// Sequence number of the moment the player (re)started playing.
    playing_seq: u64,
    /// Sequence number of the last change that was not a position update.
    changed_seq: u64,
}

#[derive(Default)]
pub(crate) struct Model {
    pub available: bool,
    entries: BTreeMap<String, Entry>,
    pinned: Option<String>,
    seq: u64,
}

impl Model {
    /// An empty model for a live bus connection.
    pub fn connected() -> Self {
        Self { available: true, ..Default::default() }
    }

    fn bump(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    fn entry_mut(&mut self, name: &str, owner: &str) -> Option<&mut Entry> {
        self.entries.get_mut(name).filter(|e| e.owner == owner)
    }

    /// A watcher finished its first full read: the player becomes visible.
    pub fn insert(&mut self, name: &str, owner: &str, root: RootProps, props: PlayerProps, queue: Option<Vec<Track>>) {
        let seq = self.bump();
        let mut last = props.clone();
        last.position = None;
        let player = Player {
            bus_name: name.to_string(),
            identity: root.identity,
            desktop_entry: root.desktop_entry,
            status: props.status,
            track: props.track,
            position: props.position.unwrap_or_default(),
            can_play: props.can_play,
            can_pause: props.can_pause,
            can_go_next: props.can_go_next,
            can_go_previous: props.can_go_previous,
            can_seek: props.can_seek,
            can_control: props.can_control,
            volume: props.volume,
            has_track_list: queue.is_some(),
            queue: queue.unwrap_or_default(),
        };
        let playing_seq = if player.status == PlaybackStatus::Playing { seq } else { 0 };
        self.entries.insert(name.to_string(), Entry { player, owner: owner.to_string(), last, playing_seq, changed_seq: seq });
    }

    /// The name lost its owner (`owner`, or whoever owns it when `None`).
    pub fn remove(&mut self, name: &str, owner: Option<&str>) -> bool {
        if self.entries.get(name).is_some_and(|e| owner.is_none_or(|o| e.owner == o)) {
            self.entries.remove(name);
            if self.pinned.as_deref() == Some(name) {
                self.pinned = None;
            }
            true
        } else {
            false
        }
    }

    pub fn update_root(&mut self, name: &str, owner: &str, root: RootProps) -> bool {
        let seq = self.bump();
        let Some(e) = self.entry_mut(name, owner) else { return false };
        if e.player.identity == root.identity && e.player.desktop_entry == root.desktop_entry {
            return false;
        }
        e.player.identity = root.identity;
        e.player.desktop_entry = root.desktop_entry;
        e.changed_seq = seq;
        true
    }

    /// New `GetAll(Player)`. Returns whether anything but the position changed.
    pub fn update_props(&mut self, name: &str, owner: &str, props: PlayerProps) -> bool {
        let seq = self.bump();
        let Some(e) = self.entry_mut(name, owner) else { return false };
        let mut sig = props.clone();
        sig.position = None;
        let changed = sig != e.last;
        let was_playing = e.player.status == PlaybackStatus::Playing;
        let track_changed = props.track.id != e.player.track.id || props.track.title != e.player.track.title;
        if changed {
            e.player.status = props.status;
            e.player.track = props.track;
            e.player.can_play = props.can_play;
            e.player.can_pause = props.can_pause;
            e.player.can_go_next = props.can_go_next;
            e.player.can_go_previous = props.can_go_previous;
            e.player.can_seek = props.can_seek;
            e.player.can_control = props.can_control;
            e.player.volume = props.volume;
            e.last = sig;
            e.changed_seq = seq;
            if e.player.status == PlaybackStatus::Playing && !was_playing {
                e.playing_seq = seq;
            }
        }
        let new_pos = match props.position {
            Some(p) => p,
            None if track_changed => Duration::ZERO,
            None => e.player.position,
        };
        let pos_changed = new_pos != e.player.position;
        e.player.position = new_pos;
        changed || pos_changed
    }

    pub fn update_position(&mut self, name: &str, owner: &str, position: Duration) -> bool {
        let Some(e) = self.entry_mut(name, owner) else { return false };
        let changed = e.player.position != position;
        e.player.position = position;
        changed
    }

    /// `None`: the player has no (readable) track list.
    pub fn update_queue(&mut self, name: &str, owner: &str, queue: Option<Vec<Track>>) -> bool {
        let seq = self.bump();
        let Some(e) = self.entry_mut(name, owner) else { return false };
        let has = queue.is_some();
        let queue = queue.unwrap_or_default();
        if e.player.has_track_list == has && e.player.queue == queue {
            return false;
        }
        e.player.has_track_list = has;
        e.player.queue = queue;
        e.changed_seq = seq;
        true
    }

    /// Pin `name` (only a player that exists) or unpin. Returns whether the pin changed.
    pub fn set_pinned(&mut self, name: Option<String>) -> bool {
        let name = name.filter(|n| self.entries.contains_key(n));
        let changed = name != self.pinned;
        self.pinned = name;
        changed
    }

    /// The pinned player while it exists; else the player that started playing most
    /// recently; else the one that changed most recently.
    pub fn active(&self) -> Option<&str> {
        if let Some(p) = self.pinned.as_deref().filter(|p| self.entries.contains_key(*p)) {
            return Some(p);
        }
        let playing = self
            .entries
            .iter()
            .filter(|(_, e)| e.player.status == PlaybackStatus::Playing)
            .max_by_key(|(_, e)| e.playing_seq);
        let best = playing.or_else(|| self.entries.iter().max_by_key(|(_, e)| e.changed_seq));
        best.map(|(name, _)| name.as_str())
    }

    /// `(bus name, owner, player)` of `target`, or of the active player for `None`.
    pub fn resolve(&self, target: Option<&str>) -> Option<(&str, &str, &Player)> {
        let name = match target {
            Some(t) => t,
            None => self.active()?,
        };
        let (name, e) = self.entries.get_key_value(name)?;
        Some((name.as_str(), e.owner.as_str(), &e.player))
    }

    pub fn state(&self) -> MprisState {
        MprisState {
            available: self.available,
            players: self.entries.values().map(|e| e.player.clone()).collect(),
            active: self.active().map(str::to_string),
            pinned: self.pinned.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "org.mpris.MediaPlayer2.a";
    const B: &str = "org.mpris.MediaPlayer2.b";

    fn props(status: PlaybackStatus, title: &str) -> PlayerProps {
        PlayerProps {
            status,
            track: Track { id: Some(format!("/t/{title}")), title: title.into(), ..Default::default() },
            can_control: true,
            can_play: true,
            can_pause: true,
            ..Default::default()
        }
    }

    fn add(m: &mut Model, name: &str, owner: &str, status: PlaybackStatus) {
        m.insert(name, owner, RootProps { identity: name.into(), ..Default::default() }, props(status, "x"), None);
    }

    #[test]
    fn no_players_no_active() {
        let m = Model::default();
        assert_eq!(m.active(), None);
        assert!(!m.state().available);
        assert!(m.state().players.is_empty());
    }

    #[test]
    fn most_recently_playing_wins_then_most_recently_changed() {
        let mut m = Model::default();
        add(&mut m, A, ":1.1", PlaybackStatus::Paused);
        add(&mut m, B, ":1.2", PlaybackStatus::Paused);
        // nobody plays: B changed last
        assert_eq!(m.active(), Some(B));
        // A starts playing
        assert!(m.update_props(A, ":1.1", props(PlaybackStatus::Playing, "x")));
        assert_eq!(m.active(), Some(A));
        // B starts playing too: the most recent starter wins
        assert!(m.update_props(B, ":1.2", props(PlaybackStatus::Playing, "x")));
        assert_eq!(m.active(), Some(B));
        // B pauses: A is still playing
        assert!(m.update_props(B, ":1.2", props(PlaybackStatus::Paused, "x")));
        assert_eq!(m.active(), Some(A));
        // A pauses: nobody plays, A changed last
        assert!(m.update_props(A, ":1.1", props(PlaybackStatus::Paused, "x")));
        assert_eq!(m.active(), Some(A));
        // a metadata change of B makes it the most recently changed
        assert!(m.update_props(B, ":1.2", props(PlaybackStatus::Paused, "y")));
        assert_eq!(m.active(), Some(B));
    }

    #[test]
    fn position_updates_do_not_change_the_active_player() {
        let mut m = Model::default();
        add(&mut m, A, ":1.1", PlaybackStatus::Paused);
        add(&mut m, B, ":1.2", PlaybackStatus::Paused);
        assert_eq!(m.active(), Some(B));
        assert!(m.update_position(A, ":1.1", Duration::from_secs(5)));
        assert!(!m.update_position(A, ":1.1", Duration::from_secs(5)));
        assert_eq!(m.active(), Some(B));
        // a props refresh that only moves the position is a position update too
        let mut p = props(PlaybackStatus::Paused, "x");
        p.position = Some(Duration::from_secs(9));
        assert!(m.update_props(A, ":1.1", p));
        assert_eq!(m.active(), Some(B));
        assert_eq!(m.state().player_pos(A), Duration::from_secs(9));
    }

    #[test]
    fn pinned_player_stays_active_until_it_leaves() {
        let mut m = Model::default();
        add(&mut m, A, ":1.1", PlaybackStatus::Paused);
        add(&mut m, B, ":1.2", PlaybackStatus::Playing);
        assert_eq!(m.active(), Some(B));
        assert!(m.set_pinned(Some(A.into())));
        assert_eq!(m.active(), Some(A));
        assert_eq!(m.state().pinned.as_deref(), Some(A));
        assert!(!m.set_pinned(Some(A.into())));
        // pinning something that does not exist is a no-op
        assert!(m.set_pinned(Some("org.mpris.MediaPlayer2.ghost".into())));
        assert_eq!(m.state().pinned, None);
        assert_eq!(m.active(), Some(B));
        m.set_pinned(Some(A.into()));
        assert!(m.remove(A, None));
        assert_eq!(m.state().pinned, None);
        assert_eq!(m.active(), Some(B));
        assert!(!m.set_pinned(None));
    }

    #[test]
    fn stale_owner_events_are_ignored() {
        let mut m = Model::default();
        add(&mut m, A, ":1.1", PlaybackStatus::Paused);
        assert!(!m.update_props(A, ":1.99", props(PlaybackStatus::Playing, "z")));
        assert!(!m.update_position(A, ":1.99", Duration::from_secs(3)));
        assert!(!m.update_queue(A, ":1.99", Some(vec![])));
        assert!(!m.remove(A, Some(":1.99")));
        assert_eq!(m.state().players[0].status, PlaybackStatus::Paused);
        // a new owner replaces the entry
        add(&mut m, A, ":1.5", PlaybackStatus::Playing);
        assert_eq!(m.resolve(Some(A)).map(|(_, o, _)| o), Some(":1.5"));
        assert!(m.remove(A, Some(":1.5")));
        assert!(m.state().players.is_empty());
    }

    #[test]
    fn resolve_targets_active_unless_named() {
        let mut m = Model::default();
        add(&mut m, A, ":1.1", PlaybackStatus::Paused);
        add(&mut m, B, ":1.2", PlaybackStatus::Playing);
        assert_eq!(m.resolve(None).map(|(n, ..)| n), Some(B));
        assert_eq!(m.resolve(Some(A)).map(|(n, ..)| n), Some(A));
        assert!(m.resolve(Some("org.mpris.MediaPlayer2.ghost")).is_none());
    }

    #[test]
    fn queue_tracks_has_track_list() {
        let mut m = Model::default();
        add(&mut m, A, ":1.1", PlaybackStatus::Paused);
        assert!(!m.state().players[0].has_track_list);
        let q = vec![Track { id: Some("/t/x".into()), title: "x".into(), ..Default::default() }];
        assert!(m.update_queue(A, ":1.1", Some(q.clone())));
        assert!(!m.update_queue(A, ":1.1", Some(q)));
        let p = &m.state().players[0];
        assert!(p.has_track_list);
        assert_eq!(p.queue_index(), Some(0));
        assert!(m.update_queue(A, ":1.1", None));
        assert!(!m.state().players[0].has_track_list);
    }

    impl MprisState {
        fn player_pos(&self, name: &str) -> Duration {
            self.player(name).unwrap().position
        }
    }
}
