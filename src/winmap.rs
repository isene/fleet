//! pid → workspace mapping via X11 wire protocol.
//!
//! Walks the root window's `_NET_CLIENT_LIST`, queries each window's
//! `_NET_WM_PID` and `_NET_WM_DESKTOP`, and builds a HashMap. Best-
//! effort: apps that don't set `_NET_WM_PID` (or set it to a wrong
//! pid) are simply absent from the map. This is fine for the use case
//! — we mainly want glass / firefox / slack attribution.

use std::collections::HashMap;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ClientMessageEvent, ConnectionExt as _, EventMask, KeyButMask,
    KeyPressEvent, KEY_PRESS_EVENT, KEY_RELEASE_EVENT,
};
use x11rb::rust_connection::RustConnection;

pub struct WinMap {
    conn: RustConnection,
    root: u32,
    atom_client_list: Atom,
    atom_wm_pid: Atom,
    atom_wm_desktop: Atom,
    atom_current_desktop: Atom,
    atom_active: Atom,
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
        let atom_active = intern(&conn, b"_NET_ACTIVE_WINDOW")?;
        Some(WinMap {
            conn,
            root,
            atom_client_list,
            atom_wm_pid,
            atom_wm_desktop,
            atom_current_desktop,
            atom_active,
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

    /// {pid → window xid}, one window per pid (first seen). Same source
    /// as refresh(), but keeps the window instead of its desktop, so a
    /// caller can raise a specific session's tab.
    pub fn pid_windows(&self) -> HashMap<u32, u32> {
        let mut out = HashMap::new();
        let windows = match self.get_atom_array(
            self.root, self.atom_client_list, AtomEnum::WINDOW.into()) {
            Some(v) if !v.is_empty() => v,
            _ => self.query_tree_root(),
        };
        let mut cookies = Vec::with_capacity(windows.len());
        for w in &windows {
            let pid = self.conn.get_property(
                false, *w, self.atom_wm_pid, AtomEnum::CARDINAL, 0, 1024);
            cookies.push((*w, pid));
        }
        for (w, pid_c) in cookies {
            if let Some(p) = first_card(pid_c) {
                out.entry(p).or_insert(w);
            }
        }
        out
    }

    /// Show and focus a window: the EWMH _NET_ACTIVE_WINDOW ClientMessage
    /// on the root. tile raises the window's tab on its workspace, and
    /// focuses it when that is the current workspace.
    pub fn activate(&self, xid: u32) {
        let ev = ClientMessageEvent::new(32, xid, self.atom_active, [2u32, 0, 0, 0, 0]);
        let _ = self.conn.send_event(
            false, self.root,
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
            ev,
        );
        let _ = self.conn.flush();
    }

    /// Switch tile to workspace `index` (0-based, so 9 is workspace 10)
    /// with the EWMH _NET_CURRENT_DESKTOP request on the root. It replaces
    /// faking tile's Mod4+N, which a held Shift turned into Mod4+Shift+N,
    /// move-to N. Needs tile v0.1.59 or later.
    pub fn switch_desktop(&self, index: u32) {
        let ev = ClientMessageEvent::new(
            32, self.root, self.atom_current_desktop, [index, 0, 0, 0, 0]);
        let _ = self.conn.send_event(
            false, self.root,
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
            ev,
        );
        let _ = self.conn.flush();
    }

    /// Type `text` into one window, then Enter, with synthetic key events.
    ///
    /// xdotool fakes real key presses (XTEST) whenever its target has the
    /// focus, and a real press merges with any key the user holds down:
    /// Mod4 plus the "h" of "Check" ran tile's Mod4+h and ate the letter.
    /// SendEvent goes to this window alone and never meets a key binding,
    /// focused or not. frame hands it to the window's owner, and glass
    /// accepts it. Printable ASCII only; false if a character had no key.
    pub fn type_line(&self, xid: u32, text: &str) -> bool {
        let setup = self.conn.setup();
        let (min, max) = (setup.min_keycode, setup.max_keycode);
        let map = match self.conn.get_keyboard_mapping(min, max - min + 1)
            .ok().and_then(|c| c.reply().ok())
        {
            Some(m) => m,
            None => return false,
        };
        let per = (map.keysyms_per_keycode as usize).max(1);
        // Latin-1 keysyms equal their character code; Return is 0xff0d.
        // Column 0 of a key is its plain symbol, column 1 its shifted one.
        let key_for = |sym: u32| {
            map.keysyms.chunks(per).enumerate().find_map(|(i, syms)| {
                let code = min + i as u8;
                if syms.first() == Some(&sym) {
                    Some((code, false))
                } else if syms.get(1) == Some(&sym) {
                    Some((code, true))
                } else {
                    None
                }
            })
        };
        let press = |code: u8, shift: bool| {
            let state = if shift { KeyButMask::SHIFT } else { KeyButMask::from(0u16) };
            for (kind, mask) in [(KEY_PRESS_EVENT, EventMask::KEY_PRESS),
                                 (KEY_RELEASE_EVENT, EventMask::KEY_RELEASE)] {
                let ev = KeyPressEvent {
                    response_type: kind, detail: code, sequence: 0, time: 0,
                    root: self.root, event: xid, child: 0,
                    root_x: 1, root_y: 1, event_x: 1, event_y: 1,
                    state, same_screen: true,
                };
                let _ = self.conn.send_event(false, xid, mask, ev);
            }
            let _ = self.conn.flush();
            // xdotool's pace. A burst can read as a paste to a TUI.
            std::thread::sleep(std::time::Duration::from_millis(12));
        };
        let mut all = true;
        for ch in text.chars() {
            let key = if (' '..='~').contains(&ch) { key_for(ch as u32) } else { None };
            match key {
                Some((code, shift)) => press(code, shift),
                None => all = false,
            }
        }
        // Enter as its own key after a beat, as the two xdotool runs did.
        std::thread::sleep(std::time::Duration::from_millis(50));
        match key_for(0xff0d) {
            Some((code, _)) => press(code, false),
            None => all = false,
        }
        all
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
