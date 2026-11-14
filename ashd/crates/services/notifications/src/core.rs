//! The pure notification store: ids, replacement, history cap, close semantics. No I/O —
//! every mutation returns the D-Bus signals the caller has to emit.

use crate::{Action, CloseReason, Notification, NotificationImage, NotificationsState, Urgency};

/// History cap; the oldest entries are dropped (`NotificationClosed(.., 4)`).
pub const HISTORY_CAP: usize = 200;

/// A parsed `Notify` call.
#[derive(Clone, Debug)]
pub struct NewNotification {
    pub app_name: String,
    pub replaces_id: u32,
    pub summary: String,
    pub body: String,
    pub actions: Vec<Action>,
    pub urgency: Urgency,
    pub image: Option<NotificationImage>,
    pub category: Option<String>,
    pub desktop_entry: Option<String>,
    pub expire_timeout_ms: Option<u32>,
    pub resident: bool,
    pub transient: bool,
    pub progress: Option<u8>,
    pub reply_placeholder: Option<String>,
    pub sync_tag: Option<String>,
    pub received: chrono::DateTime<chrono::Local>,
}

/// A signal to emit on the bus.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Closed(u32, CloseReason),
    Action(u32, String),
    Replied(u32, String),
}

#[derive(Default)]
pub struct Core {
    next_id: u32,
    next_seq: u64,
    available: bool,
    dnd: bool,
    notifications: Vec<Notification>,
}

impl Core {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn snapshot(&self) -> NotificationsState {
        NotificationsState {
            available: self.available,
            notifications: self.notifications.clone(),
            dnd: self.dnd,
            last_seq: self.next_seq,
        }
    }

    pub fn set_available(&mut self, available: bool) {
        self.available = available;
    }

    pub fn set_dnd(&mut self, dnd: bool) {
        self.dnd = dnd;
    }

    pub fn get(&self, id: u32) -> Option<&Notification> {
        self.notifications.iter().find(|n| n.id == id)
    }

    fn alloc_id(&mut self) -> u32 {
        loop {
            self.next_id = self.next_id.wrapping_add(1).max(1);
            if !self.notifications.iter().any(|n| n.id == self.next_id) {
                return self.next_id;
            }
        }
    }

    /// Insert or replace. Returns the id and the signals for entries dropped by the cap.
    pub fn notify(&mut self, req: NewNotification) -> (u32, Vec<Event>) {
        let existing = self.notifications.iter().position(|n| {
            (req.replaces_id != 0 && n.id == req.replaces_id)
                || (req.replaces_id == 0
                    && req.sync_tag.is_some()
                    && n.app_name == req.app_name
                    && n.sync_tag == req.sync_tag)
        });
        let id = match existing {
            Some(ix) => self.notifications.remove(ix).id,
            None => self.alloc_id(),
        };
        self.next_seq += 1;
        self.notifications.push(Notification {
            id,
            seq: self.next_seq,
            app_name: req.app_name,
            summary: req.summary,
            body: req.body,
            actions: req.actions,
            urgency: req.urgency,
            image: req.image,
            category: req.category,
            desktop_entry: req.desktop_entry,
            expire_timeout_ms: req.expire_timeout_ms,
            resident: req.resident,
            transient: req.transient,
            received: req.received,
            progress: req.progress,
            reply_placeholder: req.reply_placeholder,
            expired: false,
            sync_tag: req.sync_tag,
        });
        let mut events = Vec::new();
        while self.notifications.len() > HISTORY_CAP {
            let old = self.notifications.remove(0);
            if !old.closed_signalled() {
                events.push(Event::Closed(old.id, CloseReason::Undefined));
            }
        }
        (id, events)
    }

    /// Close by the given reason (see [`CloseReason`]).
    pub fn close(&mut self, id: u32, reason: CloseReason) -> Vec<Event> {
        let Some(ix) = self.notifications.iter().position(|n| n.id == id) else { return vec![] };
        if reason == CloseReason::Expired {
            let n = &mut self.notifications[ix];
            if n.expired {
                return vec![];
            }
            n.expired = true;
            if n.resident {
                // The toast is gone, the notification lives on in the history.
                return vec![];
            }
            let transient = n.transient;
            if transient {
                self.notifications.remove(ix);
            }
            return vec![Event::Closed(id, reason)];
        }
        let n = self.notifications.remove(ix);
        if n.closed_signalled() { vec![] } else { vec![Event::Closed(id, reason)] }
    }

    pub fn dismiss_all(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.notifications)
            .into_iter()
            .filter(|n| !n.closed_signalled())
            .map(|n| Event::Closed(n.id, CloseReason::Dismissed))
            .collect()
    }

    /// `ActionInvoked`, then close unless resident. Unknown keys are ignored.
    pub fn invoke(&mut self, id: u32, key: &str) -> Vec<Event> {
        let Some(n) = self.get(id) else { return vec![] };
        if !n.actions.iter().any(|a| a.key == key) {
            return vec![];
        }
        let mut events = vec![Event::Action(id, key.to_string())];
        events.extend(self.after_response(id));
        events
    }

    /// `NotificationReplied` (inline reply), then close unless resident.
    pub fn reply(&mut self, id: u32, text: String) -> Vec<Event> {
        if !self.get(id).is_some_and(|n| n.reply_placeholder.is_some()) {
            return vec![];
        }
        let mut events = vec![Event::Replied(id, text)];
        events.extend(self.after_response(id));
        events
    }

    fn after_response(&mut self, id: u32) -> Vec<Event> {
        let Some(n) = self.get(id) else { return vec![] };
        if n.resident {
            if let Some(n) = self.notifications.iter_mut().find(|n| n.id == id) {
                n.expired = true;
            }
            vec![]
        } else {
            self.close(id, CloseReason::Dismissed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn req(app: &str, summary: &str) -> NewNotification {
        NewNotification {
            app_name: app.into(),
            replaces_id: 0,
            summary: summary.into(),
            body: String::new(),
            actions: vec![],
            urgency: Urgency::Normal,
            image: None,
            category: None,
            desktop_entry: None,
            expire_timeout_ms: None,
            resident: false,
            transient: false,
            progress: None,
            reply_placeholder: None,
            sync_tag: None,
            received: chrono::Local::now(),
        }
    }

    fn act(key: &str) -> Action {
        Action { key: key.into(), label: key.into() }
    }

    #[test]
    fn ids_and_seq_are_monotonic() {
        let mut c = Core::new();
        let (a, _) = c.notify(req("x", "1"));
        let (b, _) = c.notify(req("x", "2"));
        assert_eq!((a, b), (1, 2));
        let s = c.snapshot();
        assert_eq!(s.last_seq, 2);
        assert_eq!(s.notifications.iter().map(|n| n.seq).collect::<Vec<_>>(), [1, 2]);
    }

    #[test]
    fn replace_keeps_id_bumps_seq_and_moves_to_the_end() {
        let mut c = Core::new();
        let (a, _) = c.notify(req("x", "first"));
        let (_b, _) = c.notify(req("x", "second"));
        let (a2, ev) = c.notify(NewNotification { replaces_id: a, ..req("x", "first v2") });
        assert_eq!(a2, a);
        assert!(ev.is_empty());
        let s = c.snapshot();
        assert_eq!(s.notifications.len(), 2);
        let last = s.notifications.last().unwrap();
        assert_eq!((last.id, last.seq, last.summary.as_str()), (a, 3, "first v2"));
    }

    #[test]
    fn replacing_an_unknown_id_allocates_a_new_one() {
        let mut c = Core::new();
        let (id, _) = c.notify(NewNotification { replaces_id: 99, ..req("x", "s") });
        assert_eq!(id, 1);
    }

    #[test]
    fn sync_tag_replaces_within_the_same_app_only() {
        let mut c = Core::new();
        let tag = Some("volume".to_string());
        let (a, _) = c.notify(NewNotification { sync_tag: tag.clone(), ..req("app", "50") });
        let (b, _) = c.notify(NewNotification { sync_tag: tag.clone(), ..req("app", "60") });
        let (o, _) = c.notify(NewNotification { sync_tag: tag, ..req("other", "x") });
        assert_eq!(a, b);
        assert_ne!(a, o);
        assert_eq!(c.snapshot().notifications.len(), 2);
    }

    #[test]
    fn history_is_capped() {
        let mut c = Core::new();
        let mut dropped = vec![];
        for i in 0..HISTORY_CAP + 3 {
            let (_, ev) = c.notify(req("x", &i.to_string()));
            dropped.extend(ev);
        }
        assert_eq!(c.snapshot().notifications.len(), HISTORY_CAP);
        assert_eq!(
            dropped,
            vec![
                Event::Closed(1, CloseReason::Undefined),
                Event::Closed(2, CloseReason::Undefined),
                Event::Closed(3, CloseReason::Undefined)
            ]
        );
    }

    #[test]
    fn expire_keeps_history_but_signals_once() {
        let mut c = Core::new();
        let (id, _) = c.notify(req("x", "s"));
        assert_eq!(c.close(id, CloseReason::Expired), vec![Event::Closed(id, CloseReason::Expired)]);
        assert!(c.get(id).unwrap().expired);
        assert!(c.close(id, CloseReason::Expired).is_empty());
        // dismissing it afterwards (from the notification centre) is silent
        assert!(c.close(id, CloseReason::Dismissed).is_empty());
        assert!(c.get(id).is_none());
    }

    #[test]
    fn replace_revives_an_expired_notification() {
        let mut c = Core::new();
        let (id, _) = c.notify(req("x", "s"));
        c.close(id, CloseReason::Expired);
        c.notify(NewNotification { replaces_id: id, ..req("x", "s2") });
        assert!(!c.get(id).unwrap().expired);
    }

    #[test]
    fn transient_is_not_kept_after_expiry() {
        let mut c = Core::new();
        let (id, _) = c.notify(NewNotification { transient: true, ..req("x", "s") });
        assert_eq!(c.close(id, CloseReason::Expired), vec![Event::Closed(id, CloseReason::Expired)]);
        assert!(c.get(id).is_none());
    }

    #[test]
    fn resident_expiry_is_silent() {
        let mut c = Core::new();
        let (id, _) = c.notify(NewNotification { resident: true, ..req("x", "s") });
        assert!(c.close(id, CloseReason::Expired).is_empty());
        assert!(c.get(id).unwrap().expired);
        // a user dismissal still tells the app
        assert_eq!(c.close(id, CloseReason::Dismissed), vec![Event::Closed(id, CloseReason::Dismissed)]);
    }

    #[test]
    fn close_reasons() {
        let mut c = Core::new();
        let (a, _) = c.notify(req("x", "a"));
        let (b, _) = c.notify(req("x", "b"));
        assert_eq!(c.close(a, CloseReason::Closed), vec![Event::Closed(a, CloseReason::Closed)]);
        assert_eq!(c.close(b, CloseReason::Dismissed), vec![Event::Closed(b, CloseReason::Dismissed)]);
        assert!(c.close(a, CloseReason::Closed).is_empty());
    }

    #[test]
    fn action_invocation_closes_unless_resident() {
        let mut c = Core::new();
        let (a, _) = c.notify(NewNotification { actions: vec![act("default"), act("open")], ..req("x", "a") });
        assert!(c.invoke(a, "nope").is_empty());
        assert_eq!(
            c.invoke(a, "open"),
            vec![Event::Action(a, "open".into()), Event::Closed(a, CloseReason::Dismissed)]
        );
        assert!(c.get(a).is_none());
        let (r, _) = c.notify(NewNotification { actions: vec![act("open")], resident: true, ..req("x", "r") });
        assert_eq!(c.invoke(r, "open"), vec![Event::Action(r, "open".into())]);
        assert!(c.get(r).unwrap().expired);
    }

    #[test]
    fn inline_reply_needs_a_placeholder() {
        let mut c = Core::new();
        let (a, _) = c.notify(req("x", "a"));
        assert!(c.reply(a, "hi".into()).is_empty());
        let (b, _) = c.notify(NewNotification { reply_placeholder: Some("Reply".into()), ..req("x", "b") });
        assert_eq!(
            c.reply(b, "hi".into()),
            vec![Event::Replied(b, "hi".into()), Event::Closed(b, CloseReason::Dismissed)]
        );
    }

    #[test]
    fn dismiss_all_signals_only_open_ones() {
        let mut c = Core::new();
        let (a, _) = c.notify(req("x", "a"));
        let (b, _) = c.notify(req("x", "b"));
        c.close(a, CloseReason::Expired);
        assert_eq!(c.dismiss_all(), vec![Event::Closed(b, CloseReason::Dismissed)]);
        assert!(c.snapshot().notifications.is_empty());
    }

    #[test]
    fn dnd_and_availability_are_part_of_the_snapshot() {
        let mut c = Core::new();
        c.set_dnd(true);
        c.set_available(true);
        let s = c.snapshot();
        assert!(s.dnd && s.available);
    }
}
