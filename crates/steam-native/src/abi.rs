//! Pinned private C++ object layout, called using macOS ARM64's C calling ABI.
//! No C++ source, compiler, runtime shim, or historical interface declarations.
use crate::{Result, native::Library, symbol};
use std::ffi::{CStr, c_char, c_void};
use std::ptr::NonNull;

type Object = *mut c_void;
#[repr(C)]
#[derive(Default)]
pub struct Callback {
    pub user: i32,
    pub id: i32,
    pub data: *mut u8,
    pub size: i32,
}
const _: () = assert!(std::mem::size_of::<Callback>() == 24);

struct Interface {
    object: NonNull<c_void>,
    image_base: usize,
}
impl Interface {
    fn method(&self, slot: usize, offset: usize) -> Result<*mut c_void> {
        // SAFETY: Interface is constructed only from the hash-pinned Valve
        // factory/user accessor. Its first pointer is the verified C++ vtable;
        // Callers use slots established in the research report and archived at
        // docs/evidence/steam-native-research/user-abi-map.json (repository root).
        unsafe {
            let table = *self.object.as_ptr().cast::<*const *mut c_void>();
            if table.is_null() {
                return Err("Null native vtable".into());
            }
            let address = *table.add(slot);
            let mut info: libc::Dl_info = std::mem::zeroed();
            if libc::dladdr(address, &mut info) == 0
                || info.dli_fbase as usize != self.image_base
                || (address as usize).checked_sub(self.image_base) != Some(offset)
            {
                return Err(
                    format!("Unrecognized internal ABI at slot {slot}; refusing call").into(),
                );
            }
            Ok(address)
        }
    }
}

pub struct Engine {
    interface: Interface,
    create_pipe: unsafe extern "C" fn() -> i32,
    create_global: unsafe extern "C" fn(*mut i32) -> i32,
    connect_global: unsafe extern "C" fn(i32) -> i32,
    release_pipe: unsafe extern "C" fn(i32) -> bool,
    release_user: unsafe extern "C" fn(i32, i32),
    connected: unsafe extern "C" fn(i32, i32) -> bool,
    logged_on: unsafe extern "C" fn(i32, i32) -> bool,
    next: unsafe extern "C" fn(i32, *mut Callback) -> bool,
    free: unsafe extern "C" fn(i32),
}

impl Engine {
    pub fn open(path: &std::path::Path) -> Result<Self> {
        crate::pinned_client(path)?;
        let lib = Library::open(path)?;
        println!("dlopen=ok");
        let factory = symbol!(
            lib,
            c"CreateInterface",
            unsafe extern "C" fn(*const c_char, *mut i32) -> Object
        );
        let mut engine = std::ptr::null_mut();
        for name in [
            c"SteamClient023",
            c"SteamClient020",
            c"CLIENTENGINE_INTERFACE_VERSION005",
        ] {
            let mut code = -1;
            // SAFETY: factory export signature; static version strings and valid output scalar.
            let object = unsafe { factory(name.as_ptr(), &mut code) };
            println!("factory name={} code={code}", name.to_string_lossy());
            if name == c"CLIENTENGINE_INTERFACE_VERSION005" {
                engine = object;
            }
        }
        let object = NonNull::new(engine).ok_or("Client engine interface unavailable")?;
        // SAFETY: dladdr accepts the exported factory pointer and initialized output.
        let base = unsafe {
            let mut info: libc::Dl_info = std::mem::zeroed();
            if libc::dladdr(factory as *const c_void, &mut info) == 0 {
                return Err("Cannot identify engine image".into());
            }
            info.dli_fbase as usize
        };
        Ok(Self {
            interface: Interface {
                object,
                image_base: base,
            },
            create_pipe: symbol!(lib, c"Steam_CreateSteamPipe", unsafe extern "C" fn() -> i32),
            create_global: symbol!(
                lib,
                c"Steam_CreateGlobalUser",
                unsafe extern "C" fn(*mut i32) -> i32
            ),
            connect_global: symbol!(
                lib,
                c"Steam_ConnectToGlobalUser",
                unsafe extern "C" fn(i32) -> i32
            ),
            release_pipe: symbol!(
                lib,
                c"Steam_BReleaseSteamPipe",
                unsafe extern "C" fn(i32) -> bool
            ),
            release_user: symbol!(lib, c"Steam_ReleaseUser", unsafe extern "C" fn(i32, i32)),
            connected: symbol!(
                lib,
                c"Steam_BConnected",
                unsafe extern "C" fn(i32, i32) -> bool
            ),
            logged_on: symbol!(
                lib,
                c"Steam_BLoggedOn",
                unsafe extern "C" fn(i32, i32) -> bool
            ),
            next: symbol!(
                lib,
                c"Steam_BGetCallback",
                unsafe extern "C" fn(i32, *mut Callback) -> bool
            ),
            free: symbol!(lib, c"Steam_FreeLastCallback", unsafe extern "C" fn(i32)),
        })
    }
    pub fn pipe(&self) -> i32 {
        // SAFETY: verified export with no arguments.
        unsafe { (self.create_pipe)() }
    }
    pub fn global(&self) -> (i32, i32) {
        let mut pipe = 0;
        // SAFETY: pinned export takes exactly one pointer, as confirmed by its wrapper.
        let user = unsafe { (self.create_global)(&mut pipe) };
        (user, pipe)
    }
    pub fn connect(&self, pipe: i32) -> i32 {
        // SAFETY: scalar handle returned by this engine; export validates handles.
        unsafe { (self.connect_global)(pipe) }
    }
    pub fn connected(&self, user: i32, pipe: i32) -> bool {
        // SAFETY: scalar handles returned by this engine.
        unsafe { (self.connected)(user, pipe) }
    }
    pub fn logged_on(&self, user: i32, pipe: i32) -> bool {
        // SAFETY: scalar handles returned by this engine.
        unsafe { (self.logged_on)(user, pipe) }
    }
    pub fn release(&self, user: i32, pipe: i32) {
        // SAFETY: release order matches exported API; zero handles are ignored.
        unsafe {
            if user != 0 {
                (self.release_user)(pipe, user);
            }
            if pipe != 0 {
                (self.release_pipe)(pipe);
            }
        }
    }
    pub fn callback(&self, pipe: i32) -> Option<(i32, i32)> {
        let mut event = Callback::default();
        // SAFETY: verified CallbackMsg_t layout; the borrowed payload is never
        // accessed, and every retrieved callback is freed exactly once.
        unsafe {
            if !(self.next)(pipe, &mut event) {
                return None;
            }
            let metadata = (event.id, event.size);
            (self.free)(pipe);
            Some(metadata)
        }
    }
    /// The next callback's id, freed at once. Only a payload `wanted` names,
    /// by id and exact size, is copied out first.
    pub fn callback_payload(
        &self,
        pipe: i32,
        wanted: &[(i32, usize)],
    ) -> Option<(i32, Option<Vec<u8>>)> {
        let mut event = Callback::default();
        // SAFETY: verified CallbackMsg_t layout. The engine's payload stays
        // valid until Steam_FreeLastCallback; it is read only when its id and
        // size match a requested layout, within the size the engine gave,
        // and every retrieved callback is freed exactly once.
        unsafe {
            if !(self.next)(pipe, &mut event) {
                return None;
            }
            let payload = match usize::try_from(event.size) {
                Ok(size) if !event.data.is_null() && wanted.contains(&(event.id, size)) => {
                    Some(std::slice::from_raw_parts(event.data, size).to_vec())
                }
                _ => None,
            };
            let id = event.id;
            (self.free)(pipe);
            Some((id, payload))
        }
    }
    pub fn user(&self, user: i32, pipe: i32) -> Result<ClientUser> {
        let address = self.interface.method(8, 0xa02be8)?;
        // SAFETY: pinned GetIClientUser slot, verified to take this,user,pipe.
        let object = unsafe {
            let get: unsafe extern "C" fn(Object, i32, i32) -> Object =
                std::mem::transmute(address);
            get(self.interface.object.as_ptr(), user, pipe)
        };
        Ok(ClientUser(Interface {
            object: NonNull::new(object).ok_or("Client user interface unavailable")?,
            image_base: self.interface.image_base,
        }))
    }
    pub fn friends(&self, user: i32, pipe: i32) -> Result<ClientFriends> {
        let address = self.interface.method(13, 0xa02cb4)?;
        // SAFETY: pinned Engine005 accessor takes this,user,pipe and returns
        // the Friends proxy constructed in the same native session. Evidence:
        // docs/evidence/steam-native-presence-abi.md.
        let object = unsafe {
            let get: unsafe extern "C" fn(Object, i32, i32) -> Object =
                std::mem::transmute(address);
            get(self.interface.object.as_ptr(), user, pipe)
        };
        Ok(ClientFriends(Interface {
            object: NonNull::new(object).ok_or("Client friends interface unavailable")?,
            image_base: self.interface.image_base,
        }))
    }
    /// `GetIClientUtils(pipe)`: the per-pipe Utils proxy that
    /// `Steam_CreateSteamPipe` registered for `pipe`.
    fn utils(&self, pipe: i32) -> Result<ClientUtils> {
        let address = self.interface.method(14, 0xa04d74)?;
        // SAFETY: pinned Engine005 accessor takes this,pipe and returns the
        // Utils proxy its CreateSteamPipe (0xa01e00, insert at 0xa01f94)
        // stored for that pipe. Only a pipe this engine just created reaches it.
        let object = unsafe {
            let get: unsafe extern "C" fn(Object, i32) -> Object = std::mem::transmute(address);
            get(self.interface.object.as_ptr(), pipe)
        };
        Ok(ClientUtils(Interface {
            object: NonNull::new(object).ok_or("Client utils interface unavailable")?,
            image_base: self.interface.image_base,
        }))
    }
    /// `GetIClientMatchmaking(user, pipe)`.
    fn matchmaking(&self, user: i32, pipe: i32) -> Result<ClientMatchmaking> {
        let address = self.interface.method(16, 0xa02d80)?;
        // SAFETY: pinned Engine005 accessor takes this,user,pipe, as Friends'
        // slot 13 does, and returns the Matchmaking proxy (vtable 0x1742d70)
        // of the interface set ConnectToGlobalUser built for them.
        let object = unsafe {
            let get: unsafe extern "C" fn(Object, i32, i32) -> Object =
                std::mem::transmute(address);
            get(self.interface.object.as_ptr(), user, pipe)
        };
        Ok(ClientMatchmaking(Interface {
            object: NonNull::new(object).ok_or("Client matchmaking interface unavailable")?,
            image_base: self.interface.image_base,
        }))
    }
    /// Whether the account is in `lobby` of game `app` now, as the engine's
    /// matchmaking shows it; `None` when it could not be asked this time.
    ///
    /// The engine answers a matchmaking call only for its pipe's AppID (0 on
    /// the host's own pipe), from the per-app object the game's pipe shares.
    /// So the question goes through a pipe of its own, set to `app` without
    /// process tracking, and released at once: kept open, that pipe would
    /// hold the game's lobbies after the game quits, since the engine leaves
    /// them only when the last pipe of the app goes.
    pub fn in_lobby(&self, app: u32, lobby: u64) -> Result<Option<bool>> {
        let pipe = self.pipe();
        if pipe == 0 {
            return Ok(None);
        }
        let user = self.connect(pipe);
        let answer = (|| {
            if user == 0 || self.utils(pipe)?.set_app_for_pipe(app)? != app {
                return Ok(None);
            }
            // The engine gives 0 for a lobby the account is not in, and a
            // lobby's member is an individual account.
            let first = self.matchmaking(user, pipe)?.lobby_member(lobby, 0)?;
            Ok(Some(game_runtime::protocol::individual(first)))
        })();
        // Lobby updates of the game's AppID may reach this pipe meanwhile.
        while self.callback(pipe).is_some() {}
        self.release(user, pipe);
        answer
    }
    pub fn request_app_info(&self, user: i32, pipe: i32, app: u32) -> Result<()> {
        let address = self.interface.method(17, 0xa02e4c)?;
        // SAFETY: pinned Engine005 Apps accessor and RequestAppInfoUpdate ABI
        // independently documented in steam-cloud-gamenative-bootstrap.md.
        let object = unsafe {
            let get: unsafe extern "C" fn(Object, i32, i32) -> Object =
                std::mem::transmute(address);
            get(self.interface.object.as_ptr(), user, pipe)
        };
        let apps = Interface {
            object: NonNull::new(object).ok_or("Apps unavailable")?,
            image_base: self.interface.image_base,
        };
        let address = apps.method(7, 0x8dd498)?;
        // SAFETY: one live u32 AppID and count 1, pinned proxy takes a bool result.
        let accepted = unsafe {
            let request: unsafe extern "C" fn(Object, *const u32, i32) -> bool =
                std::mem::transmute(address);
            request(object, &app, 1)
        };
        if !accepted {
            return Err("App metadata request rejected".into());
        }
        Ok(())
    }
}

pub struct ClientUser(Interface);
macro_rules! method {
    ($self:ident, $slot:expr, $offset:expr, ($($ty:ty),*) -> $ret:ty, $($arg:expr),* $(,)?) => {{
        let address = $self.0.method($slot, $offset)?;
        // SAFETY: signature and offset matched to the pinned proxy disassembly;
        // ARM64 macOS passes C++ this as the first pointer argument.
        unsafe {
            let call: unsafe extern "C" fn(Object, $($ty),*) -> $ret = std::mem::transmute(address);
            call($self.0.object.as_ptr(), $($arg),*)
        }
    }};
}
impl ClientUser {
    pub fn login_token(&self, token: &CStr, account: &CStr) -> Result<()> {
        method!(self, 56, 0x896610, (*const c_char, *const c_char) -> (), token.as_ptr(), account.as_ptr());
        Ok(())
    }
    pub fn log_on(&self, id: u64) -> Result<i32> {
        Ok(method!(self, 1, 0x890f10, (u64) -> i32, id))
    }
    pub fn steam_id(&self) -> Result<u64> {
        Ok(method!(self, 10, 0x891bf8, () -> u64,))
    }
    pub fn state(&self) -> Result<i32> {
        Ok(method!(self, 5, 0x8914dc, () -> i32,))
    }
    pub fn subscribed(&self, app: u32) -> Result<bool> {
        Ok(method!(self, 181, 0x8a2a10, (u32) -> bool, app))
    }
    pub fn request_ticket(&self, app: u32) -> Result<bool> {
        Ok(method!(self, 69, 0x897944, (u32, bool) -> bool, app, false))
    }
    pub fn ticket_length(&self, app: u32) -> Result<u32> {
        Ok(method!(self, 103, 0x89b004, (u32) -> u32, app))
    }
    /// `AuthorizeMicroTxn(order, answer)`: the player's answer to purchase
    /// `order` (`EMicroTxnAuthResponse`: 1 authorize, 2 deny). Returns the
    /// engine's API call handle, 0 when the call failed.
    pub fn authorize_microtxn(&self, order: u64, approve: bool) -> Result<u64> {
        // The pinned proxy 0x89fde4 (vtable 0x173fe80, names IClientUser and
        // AuthorizeMicroTxn at 0x89feb4/0x89febc) serializes the u64 from x1
        // and the u32 from w2, and returns the reply's 8 bytes. The server
        // dispatcher (hash 0x32506707 at 0x8004bc) reads 8 then 4 bytes and
        // calls CUser slot 152 (`ldr x8, [x8, #0x4c0]` at 0x8005ac); its
        // implementation 0xa6b950 queues CAPIJobAuthorizeMicroTxn with the
        // order at +0x1e0 and the answer at +0x1e8. That job (0xa7984c)
        // sends both in EMsg 5505 and, for answer 2 or an accepted answer,
        // drops the order and posts its dismissal (1020044, 0xa79a48).
        Ok(method!(self, 152, 0x89fde4, (u64, u32) -> u64, order, if approve { 1 } else { 2 }))
    }
}

struct ClientUtils(Interface);
impl ClientUtils {
    /// `SetAppIDForCurrentPipe(app, false)`, for this proxy's pipe: the
    /// AppID the engine then gives it, or 0 when the call failed.
    fn set_app_for_pipe(&self, app: u32) -> Result<u32> {
        // The pinned proxy 0x8cc5c8 serializes the u32 and one bool byte and
        // returns the reply's u32. Its server (0xa139f4) maps the calling pipe
        // to the AppID and, without tracking, returns it at once: no process
        // is attached and no game is reported started.
        Ok(method!(self, 18, 0x8cc5c8, (u32, bool) -> u32, app, false))
    }
}

struct ClientMatchmaking(Interface);
impl ClientMatchmaking {
    /// `GetLobbyMemberByIndex(lobby, index)`: 0 unless the account is in
    /// `lobby` and it has that many members.
    fn lobby_member(&self, lobby: u64, index: i32) -> Result<u64> {
        // The pinned proxy 0x8d81ac serializes the CSteamID by value and the
        // i32, and returns the reply's CSteamID, or 0 when the call failed.
        // Its server (0xabbab0) reads only the lobbies the user is in.
        Ok(method!(self, 18, 0x8d81ac, (u64, i32) -> u64, lobby, index))
    }
}

pub struct ClientFriends(Interface);
impl ClientFriends {
    /// `SetPersonaState(persona_state)`: the account shows as that
    /// EPersonaState (0 offline, 1 online, 3 away, 7 invisible).
    pub fn set_presence(&self, persona_state: u8) -> Result<()> {
        // The pinned proxy serializes one i32 and returns no value. Its server
        // dispatcher independently calls Friends slot 5 with that same scalar.
        method!(self, 5, 0x8b23a0, (i32) -> (), i32::from(persona_state));
        Ok(())
    }
    /// `GetFriendGamePlayed(friend)`: the game `friend` plays as this engine
    /// knows it, or `None` when it knows none.
    pub fn game_played(&self, friend: u64) -> Result<Option<[u8; crate::lobby::SIZE]>> {
        let mut info = [0_u8; crate::lobby::SIZE];
        // The pinned proxy 0x8b4074 serializes the CSteamID by value (x1),
        // reads exactly 24 bytes of the reply into the buffer in x2, or
        // zeroes them, and returns the reply's bool.
        let played = method!(self, 23, 0x8b4074, (u64, *mut u8) -> bool, friend, info.as_mut_ptr());
        Ok(played.then_some(info))
    }
    /// `NotifyGameOverlayStateChanged(pid, app, active, user_initiated)`:
    /// tells game `app` Steam's overlay opened or closed over it.
    ///
    /// The pinned proxy 0x8c42bc (vtable 0x1741520 slot 192; names
    /// IClientFriends and NotifyGameOverlayStateChanged at 0x8c43bc/0x8c43c4)
    /// serializes w1 and w2 as u32, then w3 and w4 as one byte each, and
    /// decodes no value. Its server, CUserFriends slot 192 (0xa9dbdc),
    /// builds `GameOverlayActivated_t` (331, 12 bytes): active at 0,
    /// user_initiated at 1, w2 at 4 (the AppID) and w1 at 8 (the overlay's
    /// pid), and posts it to every pipe of the user, this host's included.
    pub fn overlay_state(
        &self,
        pid: u32,
        app: u32,
        active: bool,
        user_initiated: bool,
    ) -> Result<()> {
        method!(self, 192, 0x8c42bc, (u32, u32, bool, bool) -> (), pid, app, active, user_initiated);
        Ok(())
    }
    /// `NotifyLobbyJoinRequested(app, lobby, friend)`: game `app` gets
    /// `GameLobbyJoinRequested_t` (333), as when the player accepts
    /// `friend`'s lobby invitation in Steam's overlay.
    ///
    /// The pinned proxy 0x8c45e4 (slot 194; names NotifyLobbyJoinRequested
    /// at 0x8c46c4) serializes w1 as u32 and x2, x3 as CSteamIDs by value
    /// (0xcc430 writes the 8 bytes of x1), and decodes a one-byte result.
    /// Its server (0xa9ddcc) stores x2 at 0 and x3 at 8 and posts the 16
    /// bytes through 0x5c56a4, only to pipes whose AppID is w1.
    pub fn lobby_join_requested(&self, app: u32, lobby: u64, friend: u64) -> Result<bool> {
        Ok(method!(self, 194, 0x8c45e4, (u32, u64, u64) -> bool, app, lobby, friend))
    }
    /// `NotifyRichPresenceJoinRequested(app, friend, connect)`: game `app`
    /// gets `GameRichPresenceJoinRequested_t` (337).
    ///
    /// The pinned proxy 0x8c4790 (slot 195; names
    /// NotifyRichPresenceJoinRequested at 0x8c4870) serializes w1 as u32, x2
    /// by value and x3 as a C string (0xcc1e4: strlen + 1 bytes after a length
    /// byte, 255 meaning NUL-terminated), and decodes a one-byte result. The
    /// dispatcher (0x819120) reads them back with 0xcc254 and calls slot 195
    /// (0x819228, offset 0x618); the server (0xa9ddfc) copies at most 256
    /// bytes of the string after the friend and posts 264 bytes to pipes
    /// whose AppID is w1.
    pub fn rich_presence_join_requested(
        &self,
        app: u32,
        friend: u64,
        connect: &CStr,
    ) -> Result<bool> {
        Ok(
            method!(self, 195, 0x8c4790, (u32, u64, *const c_char) -> bool, app, friend, connect.as_ptr()),
        )
    }
}
