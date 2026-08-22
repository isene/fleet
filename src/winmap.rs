//! pid → workspace mapping via X11 wire protocol.
//!
//! Walks the root window's `_NET_CLIENT_LIST`, queries each window's
//! `_NET_WM_PID` and `_NET_WM_DESKTOP`, and builds a HashMap. Best-
//! effort: apps that don't set `_NET_WM_PID` (or set it to a wrong
//! pid) are simply absent from the map. This is fine for the use case
//! — we mainly want glass / firefox / slack attribution.

use std::collections::HashMap;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{Atom, AtomEnum, ConnectionExt as _};
use x11rb::rust_connection::RustConnection;

pub struct WinMap {
    conn: RustConnection,
    root: u32,
    atom_client_list: Atom,
    atom_wm_pid: Atom,
    atom_wm_desktop: Atom,
    atom_current_desktop: Atom,
}

impl WinMap {
    pub fn connect() -> Option<Self> {
        let (conn, screen_num) = RustConnection::connect(None).ok()?;
        let setup = conn.setup();
        let root = setup.roots[screen_num].root;
        let atom_client_list = intern(&conn, b"_NET_CLIENT_LIST")?;
        let atom_wm_pid = intern(&conn, b"_NET_WM_PID")?;
        let atom_wm_desktop = intern(&conn, b"_NET_WM_DESKTOP")?;
        let atom_current_desktop = intern(&conn, b"_NET_CURRENT_DESKTOP")?;
        Some(WinMap {
            conn,
            root,
            atom_client_list,
            atom_wm_pid,
            atom_wm_desktop,
            atom_current_desktop,
        })
    }

    /// The workspace currently shown (root's _NET_CURRENT_DESKTOP).
    pub fn current_desktop(&self) -> Option<u32> {
        self.get_atom_array(self.root, self.atom_current_desktop,
                            AtomEnum::CARDINAL.into())
            .and_then(|v| v.first().copied())
    }

    /// Build a {pid → workspace_index} map. Tries _NET_CLIENT_LIST
    /// first (EWMH-standard, what most WMs publish) and falls back
    /// to walking root's direct children via QueryTree (works on
    /// minimal WMs like tile that don't publish the client list).
    /// Workspace numbering matches the per-window _NET_WM_DESKTOP
    /// atom — 0-based.
    pub fn refresh(&self) -> HashMap<u32, u32> {
        let mut out = HashMap::new();
        let windows = match self.get_atom_array(self.root, self.atom_client_list, AtomEnum::WINDOW.into()) {
            Some(v) if !v.is_empty() => v,
            _ => self.query_tree_root(),
        };
        // Ask for everything first, then read the answers. Asking and
        // waiting per window costs two blocking round trips each, which
        // is ~370 wakeups per pass on a busy desktop; queued this way it
        // is one flush and one stream of replies.
        let mut cookies = Vec::with_capacity(windows.len());
        for w in &windows {
            let pid = self.conn.get_property(
                false, *w, self.atom_wm_pid, AtomEnum::CARDINAL, 0, 1024);
            let desk = self.conn.get_property(
                false, *w, self.atom_wm_desktop, AtomEnum::CARDINAL, 0, 1024);
            cookies.push((pid, desk));
        }
        for (pid_c, desk_c) in cookies {
            let pid = first_card(pid_c);
            let desk = first_card(desk_c);
            if let Some(p) = pid {
                // Multi-window apps (Firefox spawns ~8 hidden helpers
                // sharing the same _NET_WM_PID) confuse a naive
                // first-wins map: the WM only sets _NET_WM_DESKTOP on
                // its tracked top-levels, not on 1×1 popup helpers.
                // Always prefer a real desktop value over the missing
                // sentinel; if every window for a pid lacks the
                // property, fall back to the sentinel.
                match desk {
                    Some(d) => {
                        out.insert(p, d);
                    }
                    None => {
                        out.entry(p).or_insert(u32::MAX);
                    }
                }
            }
        }
        out
    }

    fn query_tree_root(&self) -> Vec<u32> {
        let reply = match self.conn.query_tree(self.root) {
            Ok(c) => c.reply().ok(),
            Err(_) => None,
        };
        match reply {
            Some(r) => r.children,
            None => Vec::new(),
        }
    }

    fn get_atom_array(&self, win: u32, prop: Atom, ty: Atom) -> Option<Vec<u32>> {
        let reply = self
            .conn
            .get_property(false, win, prop, ty, 0, 1024)
            .ok()?
            .reply()
            .ok()?;
        if reply.format != 32 {
            return None;
        }
        Some(
            reply
                .value
                .chunks_exact(4)
                .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
                .collect(),
        )
    }
}

/// First CARDINAL of a queued GetProperty reply, or None for anything
/// that did not come back as a 32-bit property.
fn first_card<C>(cookie: Result<x11rb::cookie::Cookie<'_, RustConnection,
        x11rb::protocol::xproto::GetPropertyReply>, C>) -> Option<u32> {
    let reply = cookie.ok()?.reply().ok()?;
    if reply.format != 32 {
        return None;
    }
    reply.value.chunks_exact(4).next()
        .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
}

fn intern(conn: &RustConnection, name: &[u8]) -> Option<Atom> {
    Some(conn.intern_atom(false, name).ok()?.reply().ok()?.atom)
}
