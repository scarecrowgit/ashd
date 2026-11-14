//! D-Bus values → plain data. Players are sloppy about types (`mpris:length` as `x`, `t`
//! or `i`; `xesam:artist` as `as` or `s`; `mpris:trackid` as `o` or `s`), so everything
//! here is lenient and never fails: a missing or odd value is simply absent.

use std::{collections::HashMap, path::PathBuf, time::Duration};

use zvariant::{OwnedValue, Value};

use crate::{PlaybackStatus, Track};

pub(crate) type Props = HashMap<String, OwnedValue>;

/// The path MPRIS uses for "no track".
pub(crate) const NO_TRACK: &str = "/org/mpris/MediaPlayer2/TrackList/NoTrack";

fn unwrap<'a, 'b>(v: &'a Value<'b>) -> &'a Value<'b> {
    match v {
        Value::Value(inner) => unwrap(inner),
        v => v,
    }
}

fn get<'a>(map: &'a Props, key: &str) -> Option<&'a Value<'static>> {
    map.get(key).map(|v| unwrap(v))
}

fn as_string(v: &Value<'_>) -> Option<String> {
    match unwrap(v) {
        Value::Str(s) => Some(s.to_string()),
        Value::ObjectPath(p) => Some(p.to_string()),
        _ => None,
    }
}

fn as_i64(v: &Value<'_>) -> Option<i64> {
    match unwrap(v) {
        Value::U8(v) => Some((*v).into()),
        Value::I16(v) => Some((*v).into()),
        Value::U16(v) => Some((*v).into()),
        Value::I32(v) => Some((*v).into()),
        Value::U32(v) => Some((*v).into()),
        Value::I64(v) => Some(*v),
        Value::U64(v) => i64::try_from(*v).ok(),
        Value::F64(v) if v.is_finite() => Some(*v as i64),
        _ => None,
    }
}

fn as_f64(v: &Value<'_>) -> Option<f64> {
    match unwrap(v) {
        Value::F64(v) => Some(*v),
        other => as_i64(other).map(|i| i as f64),
    }
}

fn as_bool(v: &Value<'_>) -> Option<bool> {
    match unwrap(v) {
        Value::Bool(b) => Some(*b),
        _ => None,
    }
}

fn as_strings(v: &Value<'_>) -> Vec<String> {
    match unwrap(v) {
        Value::Array(a) => a.iter().filter_map(as_string).filter(|s| !s.is_empty()).collect(),
        other => as_string(other).filter(|s| !s.is_empty()).into_iter().collect(),
    }
}

/// Microseconds → `Duration`; negative values (players use -1 for "unknown") are absent.
fn micros(v: &Value<'_>) -> Option<Duration> {
    as_i64(v).and_then(|us| u64::try_from(us).ok()).map(Duration::from_micros)
}

/// A `Metadata` dictionary (or one `GetTracksMetadata` entry).
pub(crate) fn parse_track(md: &Props) -> Track {
    let id = get(md, "mpris:trackid").and_then(as_string).filter(|s| !s.is_empty());
    Track {
        id,
        title: get(md, "xesam:title").and_then(as_string).unwrap_or_default(),
        artists: get(md, "xesam:artist").map(as_strings).unwrap_or_default(),
        album: get(md, "xesam:album").and_then(as_string).unwrap_or_default(),
        art_url: get(md, "mpris:artUrl").and_then(as_string).filter(|s| !s.is_empty()),
        art_path: None,
        length: get(md, "mpris:length").and_then(micros).filter(|d| !d.is_zero()),
        url: get(md, "xesam:url").and_then(as_string).filter(|s| !s.is_empty()),
    }
}

/// The `Metadata` entry of a `GetAll` result as a [`Props`].
fn metadata(props: &Props) -> Props {
    props.get("Metadata").and_then(|v| Props::try_from(v.try_clone().ok()?).ok()).unwrap_or_default()
}

/// What `GetAll(org.mpris.MediaPlayer2.Player)` says (position excluded from change
/// detection by the model).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PlayerProps {
    pub status: PlaybackStatus,
    pub track: Track,
    pub position: Option<Duration>,
    pub can_play: bool,
    pub can_pause: bool,
    pub can_go_next: bool,
    pub can_go_previous: bool,
    pub can_seek: bool,
    pub can_control: bool,
    pub volume: Option<f64>,
}

pub(crate) fn parse_player(props: &Props) -> PlayerProps {
    let flag = |key: &str, default: bool| get(props, key).and_then(as_bool).unwrap_or(default);
    let can_control = flag("CanControl", false);
    PlayerProps {
        status: get(props, "PlaybackStatus").and_then(as_string).map(|s| PlaybackStatus::parse(&s)).unwrap_or_default(),
        track: parse_track(&metadata(props)),
        position: get(props, "Position").and_then(micros),
        // The spec says the Can* flags are only meaningful if CanControl; players that
        // omit one of them still accept the call, so a missing flag counts as `can_control`.
        can_play: flag("CanPlay", can_control),
        can_pause: flag("CanPause", can_control),
        can_go_next: flag("CanGoNext", can_control),
        can_go_previous: flag("CanGoPrevious", can_control),
        can_seek: flag("CanSeek", false),
        can_control,
        volume: get(props, "Volume").and_then(as_f64).filter(|v| v.is_finite() && *v >= 0.0),
    }
}

/// `GetAll(org.mpris.MediaPlayer2)`.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RootProps {
    pub identity: String,
    pub desktop_entry: Option<String>,
    pub has_track_list: bool,
}

pub(crate) fn parse_root(props: &Props) -> RootProps {
    RootProps {
        identity: get(props, "Identity").and_then(as_string).unwrap_or_default(),
        desktop_entry: get(props, "DesktopEntry")
            .and_then(as_string)
            .map(|s| s.strip_suffix(".desktop").map(str::to_string).unwrap_or(s))
            .filter(|s| !s.is_empty()),
        has_track_list: get(props, "HasTrackList").and_then(as_bool).unwrap_or(false),
    }
}

/// `file:///music/a%20b.png` → `/music/a b.png`. Anything but a `file:` URL is `None`.
pub fn art_path_from_url(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    // `file://localhost/x` and `file:///x`; a URL without an absolute path is useless.
    let path = rest.strip_prefix("localhost").unwrap_or(rest);
    if !path.starts_with('/') {
        return None;
    }
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = path.get(i + 1..i + 3)
            && let Ok(b) = u8::from_str_radix(hex, 16)
        {
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    let path = String::from_utf8(out).ok()?;
    // Drop any query / fragment a sloppy player appended.
    let path = path.split(['?', '#']).next().unwrap_or(&path);
    Some(PathBuf::from(path))
}

/// A track id that can be passed to `SetPosition` (a valid object path, not "no track").
pub(crate) fn usable_track_id(id: &str) -> Option<zvariant::ObjectPath<'_>> {
    if id == NO_TRACK {
        return None;
    }
    zvariant::ObjectPath::try_from(id).ok()
}

#[cfg(test)]
mod tests {
    use zvariant::{ObjectPath, Value};

    use super::*;

    fn props(items: Vec<(&str, Value<'static>)>) -> Props {
        items.into_iter().map(|(k, v)| (k.to_string(), v.try_to_owned().unwrap())).collect()
    }

    #[test]
    fn art_urls() {
        assert_eq!(art_path_from_url("file:///tmp/a%20b.png"), Some(PathBuf::from("/tmp/a b.png")));
        assert_eq!(art_path_from_url("file://localhost/tmp/x.jpg"), Some(PathBuf::from("/tmp/x.jpg")));
        assert_eq!(art_path_from_url("file:///tmp/x.png?v=2"), Some(PathBuf::from("/tmp/x.png")));
        assert_eq!(art_path_from_url("file:///tmp/100%zz"), Some(PathBuf::from("/tmp/100%zz")));
        assert_eq!(art_path_from_url("https://example.org/x.png"), None);
        assert_eq!(art_path_from_url("file://relative"), None);
        assert_eq!(art_path_from_url(""), None);
    }

    #[test]
    fn track_from_sloppy_metadata() {
        let md = props(vec![
            ("mpris:trackid", Value::from(ObjectPath::try_from("/t/1").unwrap())),
            ("xesam:title", Value::from("Song")),
            ("xesam:artist", Value::from(vec!["A", "B", ""])),
            ("xesam:album", Value::from("Album")),
            ("mpris:length", Value::from(200_000_000u64)),
            ("mpris:artUrl", Value::from("file:///tmp/c.png")),
        ]);
        let t = parse_track(&md);
        assert_eq!(t.id.as_deref(), Some("/t/1"));
        assert_eq!(t.title, "Song");
        assert_eq!(t.artists, ["A", "B"]);
        assert_eq!(t.album, "Album");
        assert_eq!(t.length, Some(Duration::from_secs(200)));
        assert_eq!(t.artist_line(), "A, B");

        // artist as a plain string, id as a plain string, unknown (-1) length
        let md = props(vec![
            ("mpris:trackid", Value::from("spotify:track:1")),
            ("xesam:artist", Value::from("Solo")),
            ("mpris:length", Value::from(-1i64)),
        ]);
        let t = parse_track(&md);
        assert_eq!(t.id.as_deref(), Some("spotify:track:1"));
        assert_eq!(t.artists, ["Solo"]);
        assert_eq!(t.length, None);
        assert!(parse_track(&Props::new()).is_blank());
    }

    #[test]
    fn capability_flags_default_to_can_control() {
        let p = parse_player(&props(vec![("CanControl", Value::from(true)), ("PlaybackStatus", Value::from("Playing"))]));
        assert!(p.can_play && p.can_pause && p.can_go_next && p.can_go_previous);
        assert!(!p.can_seek);
        assert_eq!(p.status, PlaybackStatus::Playing);
        let p = parse_player(&props(vec![("CanControl", Value::from(false))]));
        assert!(!p.can_play && !p.can_go_next);
        let p = parse_player(&props(vec![
            ("CanControl", Value::from(true)),
            ("CanGoNext", Value::from(false)),
            ("Volume", Value::from(0.5f64)),
            ("Position", Value::from(1_500_000i64)),
        ]));
        assert!(p.can_play && !p.can_go_next);
        assert_eq!(p.volume, Some(0.5));
        assert_eq!(p.position, Some(Duration::from_millis(1500)));
    }

    #[test]
    fn root_props() {
        let r = parse_root(&props(vec![
            ("Identity", Value::from("Firefox")),
            ("DesktopEntry", Value::from("firefox.desktop")),
            ("HasTrackList", Value::from(true)),
        ]));
        assert_eq!(r.identity, "Firefox");
        assert_eq!(r.desktop_entry.as_deref(), Some("firefox"));
        assert!(r.has_track_list);
    }

    #[test]
    fn usable_ids() {
        assert!(usable_track_id("/org/mpris/MediaPlayer2/Track/3").is_some());
        assert!(usable_track_id(NO_TRACK).is_none());
        assert!(usable_track_id("spotify:track:1").is_none());
    }
}
