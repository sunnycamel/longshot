use std::cell::RefCell;
use std::ffi::c_void;
use std::ptr;

type OSStatus = i32;
type EventTargetRef = *mut c_void;
type EventHandlerRef = *mut c_void;
type EventHandlerCallRef = *mut c_void;
type EventRef = *mut c_void;
type EventHotKeyRef = *mut c_void;
type EventHandlerUPP = unsafe extern "C" fn(EventHandlerCallRef, EventRef, *mut c_void) -> OSStatus;

#[repr(C)]
struct EventTypeSpec {
    event_class: u32,
    event_kind: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct EventHotKeyID {
    signature: u32,
    id: u32,
}

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    fn GetApplicationEventTarget() -> EventTargetRef;
    fn InstallEventHandler(
        target: EventTargetRef,
        handler: EventHandlerUPP,
        num_types: usize,
        list: *const EventTypeSpec,
        user_data: *mut c_void,
        out_ref: *mut EventHandlerRef,
    ) -> OSStatus;
    fn RegisterEventHotKey(
        key_code: u32,
        modifiers: u32,
        hot_key_id: EventHotKeyID,
        target: EventTargetRef,
        options: u32,
        out_ref: *mut EventHotKeyRef,
    ) -> OSStatus;
    fn UnregisterEventHotKey(hot_key: EventHotKeyRef) -> OSStatus;
}

const K_EVENT_CLASS_KEYBOARD: u32 = 0x6B657962; // 'keyb'
const K_EVENT_HOT_KEY_PRESSED: u32 = 5;
const ESCAPE_KEY_CODE: u32 = 53;

thread_local! {
    static ESC_CALLBACK: RefCell<Option<Box<dyn Fn()>>> = RefCell::new(None);
}

unsafe extern "C" fn hotkey_handler(
    _call_ref: EventHandlerCallRef,
    _event: EventRef,
    _user_data: *mut c_void,
) -> OSStatus {
    ESC_CALLBACK.with(|callback| {
        if let Some(callback) = callback.borrow().as_ref() {
            callback();
        }
    });
    0
}

pub fn install_handler(callback: Box<dyn Fn()>) -> bool {
    ESC_CALLBACK.with(|slot| {
        *slot.borrow_mut() = Some(callback);
    });
    let spec = EventTypeSpec {
        event_class: K_EVENT_CLASS_KEYBOARD,
        event_kind: K_EVENT_HOT_KEY_PRESSED,
    };
    let mut handler_ref: EventHandlerRef = ptr::null_mut();
    let status = unsafe {
        InstallEventHandler(
            GetApplicationEventTarget(),
            hotkey_handler,
            1,
            &spec,
            ptr::null_mut(),
            &mut handler_ref,
        )
    };
    status == 0
}

pub struct EscapeHotKey {
    reference: EventHotKeyRef,
}

impl EscapeHotKey {
    pub fn register() -> Option<Self> {
        let mut reference: EventHotKeyRef = ptr::null_mut();
        let status = unsafe {
            RegisterEventHotKey(
                ESCAPE_KEY_CODE,
                0,
                EventHotKeyID { signature: 0x4C53484B, id: 1 }, // 'LSHK'
                GetApplicationEventTarget(),
                0,
                &mut reference,
            )
        };
        if status == 0 && !reference.is_null() {
            Some(Self { reference })
        } else {
            None
        }
    }
}

impl Drop for EscapeHotKey {
    fn drop(&mut self) {
        if !self.reference.is_null() {
            unsafe {
                UnregisterEventHotKey(self.reference);
            }
        }
    }
}
