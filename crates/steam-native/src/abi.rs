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
}
