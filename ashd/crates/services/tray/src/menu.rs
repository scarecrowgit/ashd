//! The `com.canonical.dbusmenu` client: layout fetch (with lazily filled submenus) and
//! events. Every call has a deadline: a hung app costs its own menu, never the service.

use std::{collections::HashMap, time::Duration};

use zbus::{Connection, zvariant::OwnedValue};
use zvariant::Value;

use crate::{
    MenuNode,
    icons::Resolver,
    model::{empty_submenus, graft, parse_layout},
    service::timeout,
};

pub(crate) const IFACE: &str = "com.canonical.dbusmenu";

/// `AboutToShow` is a courtesy: apps that take longer are not waited for.
const ABOUT_TO_SHOW: Duration = Duration::from_millis(1500);
const GET_LAYOUT: Duration = Duration::from_secs(4);
/// Whole menu fetch, grafting included.
const LOAD_TOTAL: Duration = Duration::from_secs(8);
/// Lazy submenus filled per fetch (a few rounds of at most this many).
const MAX_LAZY: usize = 24;
const LAZY_ROUNDS: usize = 3;

type Layout = (u32, (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>));

async fn about_to_show(conn: &Connection, owner: &str, path: &str, id: i32) -> bool {
    let call = async { conn.call_method(Some(owner), path, Some(IFACE), "AboutToShow", &(id,)).await };
    match smol::future::or(async { Some(call.await) }, async {
        smol::Timer::after(ABOUT_TO_SHOW).await;
        None
    })
    .await
    {
        Some(Ok(reply)) => reply.body().deserialize::<bool>().unwrap_or(false),
        _ => false,
    }
}

async fn get_layout(conn: &Connection, owner: &str, path: &str, parent: i32, r: &Resolver, theme_path: &str) -> zbus::Result<MenuNode> {
    let reply = timeout(
        GET_LAYOUT,
        conn.call_method(Some(owner), path, Some(IFACE), "GetLayout", &(parent, -1i32, Vec::<String>::new())),
    )
    .await?;
    let (_revision, (id, props, children)): Layout = reply.body().deserialize()?;
    let (r, theme_path) = (r.clone(), theme_path.to_string());
    // Icon lookups touch the disk: off the service loop.
    Ok(smol::unblock(move || parse_layout(id, &props, &children, &r, &theme_path)).await)
}

/// The whole menu of `owner`'s `path`. Submenus that came without children are asked for
/// (`AboutToShow` + `GetLayout` of that id) and grafted in.
pub(crate) async fn load(conn: &Connection, owner: &str, path: &str, r: &Resolver, theme_path: &str) -> zbus::Result<MenuNode> {
    timeout(LOAD_TOTAL, async {
        about_to_show(conn, owner, path, 0).await;
        let mut root = get_layout(conn, owner, path, 0, r, theme_path).await?;
        let mut budget = MAX_LAZY;
        for _ in 0..LAZY_ROUNDS {
            let mut lazy = Vec::new();
            empty_submenus(&root, &mut lazy);
            lazy.truncate(budget);
            if lazy.is_empty() {
                break;
            }
            budget -= lazy.len();
            for id in lazy {
                about_to_show(conn, owner, path, id).await;
                if let Ok(sub) = get_layout(conn, owner, path, id, r, theme_path).await {
                    graft(&mut root, id, &sub.children);
                }
            }
        }
        Ok(root)
    })
    .await
}

/// `Event(id, name, 0, now)`; `name` is `clicked`, `opened`, `closed`, `hovered`.
pub(crate) async fn send_event(conn: &Connection, owner: &str, path: &str, id: i32, name: &str) -> zbus::Result<()> {
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as u32);
    timeout(
        ABOUT_TO_SHOW * 2,
        conn.call_method(Some(owner), path, Some(IFACE), "Event", &(id, name, Value::I32(0), ts)),
    )
    .await
    .map(|_| ())
}

/// `AboutToShow(id)` for the UI (lazy submenus).
pub(crate) async fn prepare(conn: &Connection, owner: &str, path: &str, id: i32) {
    about_to_show(conn, owner, path, id).await;
}

/// Re-read the layout after a `LayoutUpdated` / `ItemsPropertiesUpdated`.
pub(crate) async fn reload(conn: &Connection, owner: &str, path: &str, r: &Resolver, theme_path: &str) -> zbus::Result<MenuNode> {
    // No `AboutToShow(0)` here: some apps rebuild their menu on it and signal again.
    let mut root = get_layout(conn, owner, path, 0, r, theme_path).await?;
    let mut lazy = Vec::new();
    empty_submenus(&root, &mut lazy);
    lazy.truncate(MAX_LAZY);
    for id in lazy {
        if let Ok(sub) = get_layout(conn, owner, path, id, r, theme_path).await {
            graft(&mut root, id, &sub.children);
        }
    }
    Ok(root)
}
