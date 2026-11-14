//! D-Bus values → plain data. Apps are sloppy about types and sizes, so everything here
//! is lenient and never fails: a missing or odd value is simply absent. Pure (the only
//! file-system access goes through [`Resolver`]), unit-tested without a bus.

use std::collections::HashMap;

use zvariant::{Dict, OwnedValue, Value};

use crate::{
    Category, DEFAULT_ITEM_PATH, MenuKind, MenuNode, PIXMAP_TARGET, Status, ToolTip, Toggle, TrayItem,
    icons::Resolver,
};

/// Largest edge accepted from an app (a hostile `65535×65535` pixmap must not allocate).
const MAX_EDGE: i32 = 1024;
/// Menus deeper than this are cut (a broken app cannot recurse us to death).
const MAX_MENU_DEPTH: u8 = 12;
/// …and wider than this.
const MAX_MENU_CHILDREN: usize = 512;

/// RGBA, 8 bit, straight alpha, row-major.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pixmap {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// One `(iiay)` of `IconPixmap`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RawPixmap {
    pub width: i32,
    pub height: i32,
    /// ARGB32 in network byte order: per pixel the bytes A, R, G, B.
    pub data: Vec<u8>,
}

// ── generic value helpers ─────────────────────────────────────────────────────

fn unwrap<'a, 'b>(v: &'a Value<'b>) -> &'a Value<'b> {
    match v {
        Value::Value(inner) => unwrap(inner),
        v => v,
    }
}

fn as_string(v: &Value<'_>) -> Option<String> {
    match unwrap(v) {
        Value::Str(s) => Some(s.to_string()),
        Value::ObjectPath(p) => Some(p.to_string()),
        _ => None,
    }
}

fn as_i32(v: &Value<'_>) -> Option<i32> {
    match unwrap(v) {
        Value::U8(v) => Some((*v).into()),
        Value::I16(v) => Some((*v).into()),
        Value::U16(v) => Some((*v).into()),
        Value::I32(v) => Some(*v),
        Value::U32(v) => i32::try_from(*v).ok(),
        Value::I64(v) => i32::try_from(*v).ok(),
        Value::U64(v) => i32::try_from(*v).ok(),
        _ => None,
    }
}

fn as_bool(v: &Value<'_>) -> Option<bool> {
    match unwrap(v) {
        Value::Bool(b) => Some(*b),
        // some toolkits send 0/1
        other => as_i32(other).map(|i| i != 0),
    }
}

fn as_bytes(v: &Value<'_>) -> Option<Vec<u8>> {
    let Value::Array(a) = unwrap(v) else { return None };
    a.inner().iter().map(|b| if let Value::U8(b) = b { Some(*b) } else { None }).collect()
}

type Props = HashMap<String, OwnedValue>;

fn get<'a>(map: &'a Props, key: &str) -> Option<&'a Value<'static>> {
    map.get(key).map(|v| unwrap(v))
}

fn string(map: &Props, key: &str) -> String {
    get(map, key).and_then(as_string).unwrap_or_default()
}

// ── service strings ───────────────────────────────────────────────────────────

/// `"bus"` → `("bus", "/StatusNotifierItem")`; `"bus/path"` → `("bus", "/path")`;
/// `"/path"` → `("", "/path")` (the caller supplies the sender).
pub fn parse_service(service: &str) -> (String, String) {
    match service.find('/') {
        Some(0) => (String::new(), service.to_string()),
        Some(i) => (service[..i].to_string(), service[i..].to_string()),
        None => (service.to_string(), DEFAULT_ITEM_PATH.to_string()),
    }
}

// ── pixmaps ───────────────────────────────────────────────────────────────────

/// ARGB32 (network byte order, i.e. A R G B per pixel) → RGBA. `None` for a size/length
/// mismatch or an absurd size.
pub fn pixmap_to_rgba(width: i32, height: i32, argb: &[u8]) -> Option<Pixmap> {
    if width <= 0 || height <= 0 || width > MAX_EDGE || height > MAX_EDGE {
        return None;
    }
    let (w, h) = (width as usize, height as usize);
    if argb.len() != w * h * 4 {
        return None;
    }
    let mut rgba = Vec::with_capacity(argb.len());
    for px in argb.chunks_exact(4) {
        rgba.extend_from_slice(&[px[1], px[2], px[3], px[0]]);
    }
    Some(Pixmap { width: width as u32, height: height as u32, rgba })
}

/// The pixmap nearest to [`PIXMAP_TARGET`] (the bigger one on a tie), decoded; invalid
/// and fully transparent entries are skipped.
pub(crate) fn best_pixmap(pixmaps: &[RawPixmap]) -> Option<Pixmap> {
    pixmaps
        .iter()
        .filter(|p| p.width > 0 && p.height > 0 && p.width <= MAX_EDGE && p.height <= MAX_EDGE)
        .filter(|p| p.data.len() == (p.width * p.height * 4) as usize)
        // an app's "no icon" is often an empty or all-zero pixmap
        .filter(|p| p.data.chunks_exact(4).any(|px| px[0] != 0))
        .min_by_key(|p| {
            let edge = p.width.max(p.height);
            ((edge - PIXMAP_TARGET).abs(), -edge)
        })
        .and_then(|p| pixmap_to_rgba(p.width, p.height, &p.data))
}

fn pixmaps(v: &Value<'_>) -> Vec<RawPixmap> {
    let Value::Array(a) = unwrap(v) else { return Vec::new() };
    a.inner()
        .iter()
        .filter_map(|e| {
            let Value::Structure(s) = unwrap(e) else { return None };
            let f = s.fields();
            Some(RawPixmap { width: as_i32(f.first()?)?, height: as_i32(f.get(1)?)?, data: as_bytes(f.get(2)?)? })
        })
        .collect()
}

// ── item properties ───────────────────────────────────────────────────────────

/// `GetAll` of the item interface, parsed but not yet resolved to files.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RawItem {
    pub id: String,
    pub title: String,
    pub category: String,
    pub status: String,
    pub icon_name: String,
    pub icon_theme_path: String,
    pub icon_pixmap: Vec<RawPixmap>,
    pub overlay_icon_name: String,
    pub overlay_pixmap: Vec<RawPixmap>,
    pub attention_icon_name: String,
    pub attention_pixmap: Vec<RawPixmap>,
    pub tooltip_icon_name: String,
    pub tooltip_pixmap: Vec<RawPixmap>,
    pub tooltip_title: String,
    pub tooltip_description: String,
    pub item_is_menu: bool,
    pub menu: Option<String>,
}

pub(crate) fn parse_item(props: &Props) -> RawItem {
    let mut item = RawItem {
        id: string(props, "Id"),
        title: string(props, "Title"),
        category: string(props, "Category"),
        status: string(props, "Status"),
        icon_name: string(props, "IconName"),
        icon_theme_path: string(props, "IconThemePath"),
        icon_pixmap: get(props, "IconPixmap").map(pixmaps).unwrap_or_default(),
        overlay_icon_name: string(props, "OverlayIconName"),
        overlay_pixmap: get(props, "OverlayIconPixmap").map(pixmaps).unwrap_or_default(),
        attention_icon_name: string(props, "AttentionIconName"),
        attention_pixmap: get(props, "AttentionIconPixmap").map(pixmaps).unwrap_or_default(),
        item_is_menu: get(props, "ItemIsMenu").and_then(as_bool).unwrap_or(false),
        menu: get(props, "Menu").and_then(as_string).filter(|m| m.starts_with('/') && m != "/NO_DBUSMENU"),
        ..Default::default()
    };
    // ToolTip: (icon name, [pixmap], title, description)
    if let Some(Value::Structure(s)) = get(props, "ToolTip") {
        let f = s.fields();
        item.tooltip_icon_name = f.first().and_then(as_string).unwrap_or_default();
        item.tooltip_pixmap = f.get(1).map(pixmaps).unwrap_or_default();
        item.tooltip_title = f.get(2).and_then(as_string).unwrap_or_default();
        item.tooltip_description = f.get(3).and_then(as_string).unwrap_or_default();
    }
    item
}

/// Resolve icons to files (blocking: call off the service loop). `bus` / `path` are as
/// registered, `id` is the command key.
pub(crate) fn build_item(id: &str, bus: &str, path: &str, raw: &RawItem, r: &Resolver) -> TrayItem {
    let theme_path = raw.icon_theme_path.as_str();
    let named_or_pixmap = |name: &str, px: &[RawPixmap]| {
        r.resolve(name, theme_path).or_else(|| best_pixmap(px).and_then(|p| r.store_pixmap(&p)))
    };
    TrayItem {
        id: id.to_string(),
        bus_name: bus.to_string(),
        object_path: path.to_string(),
        app_id: raw.id.clone(),
        title: raw.title.clone(),
        category: Category::parse(&raw.category),
        status: Status::parse(&raw.status),
        icon_name: raw.icon_name.clone(),
        icon_theme_path: raw.icon_theme_path.clone(),
        icon: named_or_pixmap(&raw.icon_name, &raw.icon_pixmap),
        attention_icon: named_or_pixmap(&raw.attention_icon_name, &raw.attention_pixmap),
        overlay_icon: named_or_pixmap(&raw.overlay_icon_name, &raw.overlay_pixmap),
        tooltip: ToolTip {
            title: raw.tooltip_title.clone(),
            description: raw.tooltip_description.clone(),
            icon: named_or_pixmap(&raw.tooltip_icon_name, &raw.tooltip_pixmap),
        },
        item_is_menu: raw.item_is_menu,
        menu_path: raw.menu.clone(),
    }
}

// ── dbusmenu ──────────────────────────────────────────────────────────────────

/// `_Quit` → `Quit`, `__` → `_` (dbusmenu marks mnemonics with an underscore).
pub(crate) fn strip_mnemonic(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    let mut chars = label.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '_' {
            if chars.peek() == Some(&'_') {
                out.push('_');
                chars.next();
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn shortcut_text(v: &Value<'_>) -> Option<String> {
    let Value::Array(outer) = unwrap(v) else { return None };
    let Value::Array(first) = unwrap(outer.inner().first()?) else { return None };
    let keys: Vec<String> = first
        .inner()
        .iter()
        .filter_map(as_string)
        .map(|k| match k.as_str() {
            "Control" => "Ctrl".to_string(),
            _ => k,
        })
        .collect();
    (!keys.is_empty()).then(|| keys.join("+"))
}

fn dict_map<'a, 'b>(d: &'a Dict<'b, 'b>) -> HashMap<String, &'a Value<'b>> {
    d.iter().filter_map(|(k, v)| Some((as_string(k)?, unwrap(v)))).collect()
}

fn build_node(
    id: i32,
    props: &HashMap<String, &Value<'_>>,
    children: &[&Value<'_>],
    depth: u8,
    r: &Resolver,
    theme_path: &str,
) -> MenuNode {
    let text = |k: &str| props.get(k).and_then(|v| as_string(v)).unwrap_or_default();
    let flag = |k: &str, default: bool| props.get(k).and_then(|v| as_bool(v)).unwrap_or(default);
    let kind = if text("type") == "separator" { MenuKind::Separator } else { MenuKind::Normal };
    let state = props.get("toggle-state").and_then(|v| as_i32(v)).unwrap_or(0) == 1;
    let toggle = match text("toggle-type").as_str() {
        "checkmark" => Toggle::Check(state),
        "radio" => Toggle::Radio(state),
        _ => Toggle::None,
    };
    let icon = r
        .resolve(&text("icon-name"), theme_path)
        .or_else(|| props.get("icon-data").and_then(|v| as_bytes(v)).filter(|b| !b.is_empty()).and_then(|b| r.store_png_bytes(&b)));
    let children = if depth >= MAX_MENU_DEPTH {
        Vec::new()
    } else {
        children.iter().take(MAX_MENU_CHILDREN).filter_map(|c| node_from_value(c, depth + 1, r, theme_path)).collect()
    };
    MenuNode {
        id,
        label: strip_mnemonic(&text("label")),
        enabled: flag("enabled", true),
        visible: flag("visible", true),
        kind,
        toggle,
        icon,
        shortcut: props.get("shortcut").and_then(|v| shortcut_text(v)),
        submenu: text("children-display") == "submenu",
        children,
    }
}

/// A nested `(ia{sv}av)` (the content of an `av` child).
fn node_from_value(v: &Value<'_>, depth: u8, r: &Resolver, theme_path: &str) -> Option<MenuNode> {
    let Value::Structure(s) = unwrap(v) else { return None };
    let f = s.fields();
    let id = as_i32(f.first()?)?;
    let Value::Dict(d) = unwrap(f.get(1)?) else { return None };
    let props = dict_map(d);
    let kids: Vec<&Value<'_>> = match f.get(2).map(unwrap) {
        Some(Value::Array(a)) => a.inner().iter().collect(),
        _ => Vec::new(),
    };
    Some(build_node(id, &props, &kids, depth, r, theme_path))
}

/// The top-level struct of `GetLayout` (`(ia{sv}av)` deserialised to Rust types).
pub(crate) fn parse_layout(
    id: i32,
    props: &Props,
    children: &[OwnedValue],
    r: &Resolver,
    theme_path: &str,
) -> MenuNode {
    let props: HashMap<String, &Value<'_>> = props.iter().map(|(k, v)| (k.clone(), unwrap(v))).collect();
    let kids: Vec<&Value<'_>> = children.iter().map(|c| &**c).collect();
    build_node(id, &props, &kids, 0, r, theme_path)
}

/// Replace the children of node `id` (a lazily filled submenu) with `new`.
pub(crate) fn graft(node: &mut MenuNode, id: i32, new: &[MenuNode]) -> bool {
    if node.id == id {
        node.children = new.to_vec();
        return true;
    }
    node.children.iter_mut().any(|c| graft(c, id, new))
}

/// Ids of submenu nodes that came without children (to be filled with `AboutToShow`).
pub(crate) fn empty_submenus(root: &MenuNode, out: &mut Vec<i32>) {
    for c in &root.children {
        if c.submenu && c.children.is_empty() && c.id != 0 {
            out.push(c.id);
        }
        empty_submenus(c, out);
    }
}

#[cfg(test)]
mod tests {
    use zvariant::{Array, Signature, StructureBuilder};

    use super::*;

    fn resolver() -> Resolver {
        Resolver::new(std::env::temp_dir().join("ashd-tray-model-test"))
    }

    fn v<'a>(x: impl Into<Value<'a>>) -> Value<'a> {
        x.into()
    }

    #[test]
    fn service_strings() {
        assert_eq!(parse_service(":1.42"), (":1.42".into(), "/StatusNotifierItem".into()));
        assert_eq!(parse_service(":1.42/org/ayatana/X"), (":1.42".into(), "/org/ayatana/X".into()));
        assert_eq!(parse_service("org.kde.StatusNotifierItem-5-1"), ("org.kde.StatusNotifierItem-5-1".into(), "/StatusNotifierItem".into()));
        assert_eq!(parse_service("/org/ayatana/NotificationItem/foo"), (String::new(), "/org/ayatana/NotificationItem/foo".into()));
    }

    #[test]
    fn argb_to_rgba() {
        // A R G B per pixel → R G B A
        let px = pixmap_to_rgba(2, 1, &[0xff, 1, 2, 3, 0x80, 4, 5, 6]).unwrap();
        assert_eq!((px.width, px.height), (2, 1));
        assert_eq!(px.rgba, [1, 2, 3, 0xff, 4, 5, 6, 0x80]);
        assert!(pixmap_to_rgba(2, 1, &[0; 7]).is_none(), "length mismatch");
        assert!(pixmap_to_rgba(0, 1, &[]).is_none());
        assert!(pixmap_to_rgba(-1, 1, &[0; 4]).is_none());
        assert!(pixmap_to_rgba(100_000, 100_000, &[]).is_none(), "absurd size");
    }

    fn raw(edge: i32, alpha: u8) -> RawPixmap {
        RawPixmap { width: edge, height: edge, data: [alpha, 9, 9, 9].repeat((edge * edge) as usize) }
    }

    #[test]
    fn best_pixmap_picks_nearest_size() {
        let all = vec![raw(16, 255), raw(24, 255), raw(48, 255), raw(64, 255)];
        assert_eq!(best_pixmap(&all).unwrap().width, 24, "24 is 8 away, 48 is 16");
        let all = vec![raw(16, 255), raw(48, 255)];
        assert_eq!(best_pixmap(&all).unwrap().width, 48, "equal distance: the bigger");
        assert_eq!(best_pixmap(&[raw(32, 0)]), None, "fully transparent is no icon");
        assert_eq!(best_pixmap(&[]), None);
        let broken = RawPixmap { width: 4, height: 4, data: vec![255; 3] };
        assert_eq!(best_pixmap(&[broken, raw(16, 255)]).unwrap().width, 16);
    }

    #[test]
    fn mnemonics() {
        assert_eq!(strip_mnemonic("_Quit"), "Quit");
        assert_eq!(strip_mnemonic("Save _As…"), "Save As…");
        assert_eq!(strip_mnemonic("snake__case"), "snake_case");
        assert_eq!(strip_mnemonic("plain"), "plain");
    }

    fn props(pairs: Vec<(&str, Value<'static>)>) -> Value<'static> {
        let mut d = Dict::new(&Signature::Str, &Signature::Variant);
        for (k, val) in pairs {
            d.append(v(k.to_string()), Value::Value(Box::new(val))).unwrap();
        }
        Value::Dict(d)
    }

    fn node(id: i32, pairs: Vec<(&str, Value<'static>)>, kids: Vec<Value<'static>>) -> Value<'static> {
        let mut a = Array::new(&Signature::Variant);
        for k in kids {
            a.append(Value::Value(Box::new(k))).unwrap();
        }
        let s = StructureBuilder::new().add_field(id).append_field(props(pairs)).append_field(Value::Array(a)).build().unwrap();
        Value::Structure(s)
    }

    #[test]
    fn menu_tree() {
        let tree = node(
            0,
            vec![("children-display", v("submenu".to_string()))],
            vec![
                node(1, vec![("label", v("_Open".to_string())), ("shortcut", Value::from(vec![vec!["Control".to_string(), "O".to_string()]]))], vec![]),
                node(2, vec![("type", v("separator".to_string()))], vec![]),
                node(3, vec![("label", v("Mute".to_string())), ("toggle-type", v("checkmark".to_string())), ("toggle-state", v(1i32))], vec![]),
                node(4, vec![("label", v("Pick".to_string())), ("toggle-type", v("radio".to_string())), ("toggle-state", v(0i32)), ("enabled", v(false))], vec![]),
                node(5, vec![("label", v("Hidden".to_string())), ("visible", v(false))], vec![]),
                node(
                    6,
                    vec![("label", v("More".to_string())), ("children-display", v("submenu".to_string()))],
                    vec![node(7, vec![("label", v("Deep".to_string()))], vec![])],
                ),
                node(8, vec![("label", v("Lazy".to_string())), ("children-display", v("submenu".to_string()))], vec![]),
            ],
        );
        let root = node_from_value(&tree, 0, &resolver(), "").unwrap();
        assert_eq!(root.id, 0);
        assert!(root.submenu);
        let kids = &root.children;
        assert_eq!(kids.len(), 7);
        assert_eq!(kids[0].label, "Open");
        assert_eq!(kids[0].shortcut.as_deref(), Some("Ctrl+O"));
        assert!(kids[0].enabled && kids[0].visible);
        assert_eq!(kids[1].kind, MenuKind::Separator);
        assert_eq!(kids[2].toggle, Toggle::Check(true));
        assert_eq!(kids[3].toggle, Toggle::Radio(false));
        assert!(!kids[3].enabled);
        assert!(!kids[4].visible);
        assert_eq!(root.visible_children().count(), 6);
        assert!(kids[5].is_submenu());
        assert_eq!(kids[5].children[0].label, "Deep");
        assert_eq!(root.find(7).unwrap().label, "Deep");
        assert!(kids[6].is_submenu() && kids[6].children.is_empty());

        let mut lazy = Vec::new();
        empty_submenus(&root, &mut lazy);
        assert_eq!(lazy, [8]);
        let mut root = root;
        assert!(graft(&mut root, 8, &[MenuNode { id: 9, label: "Filled".into(), ..Default::default() }]));
        assert_eq!(root.find(9).unwrap().label, "Filled");
        assert!(!graft(&mut root, 404, &[]));
    }

    #[test]
    fn menu_depth_is_bounded() {
        let mut tree = node(99, vec![], vec![]);
        for id in 0..40 {
            tree = node(id, vec![], vec![tree]);
        }
        let root = node_from_value(&tree, 0, &resolver(), "").unwrap();
        let mut depth = 0;
        let mut n = &root;
        while let Some(c) = n.children.first() {
            depth += 1;
            n = c;
        }
        assert!(depth <= MAX_MENU_DEPTH as usize, "{depth}");
    }

    #[test]
    fn item_props() {
        let mut map: Props = HashMap::new();
        let mut put = |k: &str, val: Value<'_>| {
            map.insert(k.to_string(), val.try_to_owned().unwrap());
        };
        put("Id", v("discord".to_string()));
        put("Title", v("Discord".to_string()));
        put("Category", v("Communications".to_string()));
        put("Status", v("NeedsAttention".to_string()));
        put("IconName", v("discord-tray".to_string()));
        put("ItemIsMenu", v(false));
        put("Menu", v(zvariant::ObjectPath::try_from("/MenuBar").unwrap()));
        let tt = StructureBuilder::new()
            .add_field("icon".to_string())
            .append_field(Value::Array(Array::new(&"(iiay)".parse::<Signature>().unwrap())))
            .add_field("2 unread".to_string())
            .add_field("body".to_string())
            .build()
            .unwrap();
        put("ToolTip", Value::Structure(tt));
        let raw = parse_item(&map);
        assert_eq!(raw.id, "discord");
        assert_eq!(raw.menu.as_deref(), Some("/MenuBar"));
        assert_eq!(raw.tooltip_title, "2 unread");
        assert_eq!(raw.tooltip_description, "body");
        assert_eq!(raw.tooltip_icon_name, "icon");
        let item = build_item(":1.5/Item", ":1.5", "/Item", &raw, &resolver());
        assert_eq!(item.status, Status::NeedsAttention);
        assert_eq!(item.category, Category::Communications);
        assert_eq!(item.label(), "2 unread");
        assert!(item.has_menu());
        // "no menu" spellings
        let mut none: Props = HashMap::new();
        none.insert("Menu".into(), Value::from(zvariant::ObjectPath::try_from("/NO_DBUSMENU").unwrap()).try_to_owned().unwrap());
        assert_eq!(parse_item(&none).menu, None);
    }
}
