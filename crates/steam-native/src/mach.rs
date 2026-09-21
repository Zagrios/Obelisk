//! Narrow macOS bootstrap/Mach transport, independent of Steam authentication.
use crate::Result;
use std::ffi::{CStr, CString, c_char};
pub type Port = u32;
pub const SEND: u32 = 1;
pub const RECEIVE: u32 = 2;
pub const SEND_TIMEOUT: u32 = 0x10;
pub const RECEIVE_TIMEOUT: u32 = 0x100;
pub const RECEIVE_TIMED_OUT: i32 = 0x10004003;
pub const RECEIVE_INTERRUPTED: i32 = 0x10004005;
pub const COMPLEX: u32 = 0x80000000;

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct Header {
    pub bits: u32,
    pub size: u32,
    pub remote: Port,
    pub local: Port,
    pub voucher: Port,
    pub id: i32,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Message {
    pub header: Header,
    pub opcode: u32,
    pub value: u32,
    pub path: [u8; 512],
    pub trailer: [u8; 128],
}
impl Default for Message {
    fn default() -> Self {
        Self {
            header: Header::default(),
            opcode: 0,
            value: 0,
            path: [0; 512],
            trailer: [0; 128],
        }
    }
}
const _: () = {
    assert!(std::mem::size_of::<Header>() == 24);
    assert!(std::mem::offset_of!(Message, value) == 28);
    assert!(std::mem::offset_of!(Message, path) == 32);
    assert!(std::mem::offset_of!(Message, trailer) == 544);
};

unsafe extern "C" {
    static mach_task_self_: Port;
    static bootstrap_port: Port;
    fn mach_port_allocate(task: Port, right: i32, name: *mut Port) -> i32;
    fn mach_port_insert_right(task: Port, name: Port, poly: Port, disposition: u32) -> i32;
    fn mach_port_mod_refs(task: Port, name: Port, right: i32, delta: i32) -> i32;
    fn mach_port_deallocate(task: Port, name: Port) -> i32;
    fn mach_msg(
        message: *mut Header,
        options: u32,
        send_size: u32,
        receive_limit: u32,
        receive_port: Port,
        timeout: u32,
        notify: Port,
    ) -> i32;
    fn mach_msg_destroy(message: *mut Header);
    fn bootstrap_register(bootstrap: Port, name: *const c_char, port: Port) -> i32;
    fn bootstrap_look_up(bootstrap: Port, name: *const c_char, port: *mut Port) -> i32;
}

pub struct ReceivePort(pub Port);
impl ReceivePort {
    pub fn new() -> Result<Self> {
        let mut name = 0;
        // SAFETY: current-task port and writable output scalar; receive right = 1.
        let status = unsafe { mach_port_allocate(mach_task_self_, 1, &mut name) };
        if status != 0 {
            return Err(format!("mach_port_allocate status={status}").into());
        }
        Ok(Self(name))
    }
    pub fn register(&self, name: CString) -> Result<Registration> {
        // SAFETY: owned receive right, inserted send right; name is NUL-terminated.
        unsafe {
            let status = mach_port_insert_right(mach_task_self_, self.0, self.0, 20);
            if status != 0 {
                return Err(format!("mach_port_insert_right status={status}").into());
            }
            let status = bootstrap_register(bootstrap_port, name.as_ptr(), self.0);
            // SAFETY: release the local send reference after bootstrap copies it.
            mach_port_deallocate(mach_task_self_, self.0);
            if status != 0 {
                return Err(format!("bootstrap_register status={status}").into());
            }
        }
        Ok(Registration(name))
    }
}
impl Drop for ReceivePort {
    fn drop(&mut self) {
        // SAFETY: release this object's owned receive right exactly once.
        unsafe {
            mach_port_mod_refs(mach_task_self_, self.0, 1, -1);
        }
    }
}
pub struct Registration(CString);
impl Drop for Registration {
    fn drop(&mut self) {
        // SAFETY: unregister only the unique name registered by this process.
        unsafe {
            bootstrap_register(bootstrap_port, self.0.as_ptr(), 0);
        }
    }
}
pub struct SendPort(pub Port);
impl SendPort {
    pub fn lookup(name: &CStr) -> Result<Self> {
        let mut port = 0;
        // SAFETY: valid service name and writable output scalar.
        let status = unsafe { bootstrap_look_up(bootstrap_port, name.as_ptr(), &mut port) };
        if status != 0 {
            return Err(format!("bootstrap lookup status={status}").into());
        }
        Ok(Self(port))
    }
}
impl Drop for SendPort {
    fn drop(&mut self) {
        // SAFETY: release the lookup's owned send reference.
        unsafe {
            mach_port_deallocate(mach_task_self_, self.0);
        }
    }
}
pub fn send(message: &mut Message) -> i32 {
    if message.header.size > 544 {
        return -1;
    }
    // SAFETY: bounded send size within the initialized repr(C) message.
    unsafe {
        mach_msg(
            &mut message.header,
            SEND | SEND_TIMEOUT,
            message.header.size,
            0,
            0,
            1000,
            0,
        )
    }
}
pub fn receive(message: &mut Message, port: Port, limit: u32, timeout: u32) -> i32 {
    if limit > std::mem::size_of::<Message>() as u32 {
        return -1;
    }
    // SAFETY: buffer is writable, aligned and at least limit bytes long.
    unsafe {
        mach_msg(
            &mut message.header,
            RECEIVE | RECEIVE_TIMEOUT,
            0,
            limit,
            port,
            timeout,
            0,
        )
    }
}
pub fn destroy(message: &mut Message) {
    // SAFETY: called only for a received message or a send failure retaining rights.
    unsafe {
        mach_msg_destroy(&mut message.header);
    }
}
pub fn valid_request(message: &Message) -> bool {
    message.header.id == 104
        && message.header.size == 32
        && message.header.bits & COMPLEX == 0
        && message.header.remote != 0
        && message.header.remote != u32::MAX
        && [17, 18].contains(&(message.header.bits & 0xff))
        && [100001, 14].contains(&message.opcode)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_malformed_discovery_messages() {
        let mut m = Message {
            header: Header {
                bits: 17,
                size: 32,
                remote: 123,
                id: 104,
                ..Default::default()
            },
            opcode: 14,
            ..Default::default()
        };
        assert!(valid_request(&m));
        m.header.size = 544;
        assert!(!valid_request(&m));
        m.header.size = 32;
        m.header.bits |= COMPLEX;
        assert!(!valid_request(&m));
        m.header.bits = 17;
        m.header.remote = 0;
        assert!(!valid_request(&m));
        m.header.remote = 123;
        m.opcode = 15;
        assert!(!valid_request(&m));
    }
}
