use crate::errors::KeyLayoutError;
use core::ffi::c_void;
use memmap::MmapOptions;
use std::os::unix::io::FromRawFd;
use std::{cell::RefCell, fs::File, os::raw::c_char, rc::Rc};
use std::{convert::TryInto, env};
use wayland_client::{
    protocol::{
        wl_keyboard::{KeymapFormat, WlKeyboard},
        wl_seat::WlSeat,
    },
    DispatchData, Main,
};
use xkbcommon_sys::xkb_x11_setup_xkb_extension;
use xkbcommon_sys::{
    xkb_context, xkb_context_new, xkb_keymap, xkb_keymap_format::XKB_KEYMAP_FORMAT_TEXT_v1,
    xkb_keysym_get_name, xkb_state_key_get_one_sym, xkb_state_key_get_utf8,
    xkb_x11_get_core_keyboard_device_id, XKB_CONTEXT_NO_FLAGS, XKB_KEYMAP_COMPILE_NO_FLAGS,
    XKB_X11_MIN_MAJOR_XKB_VERSION, XKB_X11_MIN_MINOR_XKB_VERSION,
    XKB_X11_SETUP_XKB_EXTENSION_NO_FLAGS,
};
use xkbcommon_sys::{xkb_keymap_new_from_buffer, xkb_x11_keymap_new_from_device};

/// The KeyLayout struct
pub struct KeyLayout {
    keymap: *mut xkb_keymap,
}

impl KeyLayout {
    /// Construct a KeyLayout from a Winit window
    pub fn new_from_window(window: &winit::window::Window) -> Result<KeyLayout, KeyLayoutError> {
        if let Some(_conn) = winit::platform::unix::WindowExtUnix::xcb_connection(window) {
            Self::_new_x11()
        } else {
            Self::_new_wayland()
        }
    }

    /// Convert a scancode to a String
    pub fn get_key_as_string(&self, scancode: u32) -> String {
        let state = unsafe { xkbcommon_sys::xkb_state_new(self.keymap) };

        // Offset of 8 between evdev scancodes and xkb scancodes

        // Get keysym from key
        let keysym = unsafe { xkb_state_key_get_one_sym(state, scancode + 8) };

        let mut buffer: [c_char; 32] = [0; 32];

        let mut key_size =
            unsafe { xkb_state_key_get_utf8(state, scancode + 8, buffer.as_mut_ptr(), 32) };

        let (utf_key, _) = buffer.split_at(key_size.try_into().unwrap());
        let mut output =
            String::from_utf8_lossy(unsafe { &*(utf_key as *const [i8] as *const [u8]) })
                .into_owned();
        // Remove invisible characters
        output = output.replace(|c: char| c.is_control(), "");

        // Fallback to longer name if needed
        if output.trim().is_empty() {
            key_size = unsafe { xkb_keysym_get_name(keysym, buffer.as_mut_ptr(), 32) };

            let (utf_key, _) = buffer.split_at(key_size.try_into().unwrap());
            output = String::from_utf8_lossy(unsafe { &*(utf_key as *const [i8] as *const [u8]) })
                .into_owned();
        } else {
            output.make_ascii_uppercase();
        }

        output
    }

    fn _new_wayland() -> Result<Self, KeyLayoutError> {
        // We try Wayland

        let display = wayland_client::Display::connect_to_env().unwrap();

        // Set up the event queue
        let mut event_queue = display.create_event_queue();
        let token = event_queue.token();

        let proxy = &*display;
        let attached = proxy.attach(token);
        let registry = attached.get_registry();

        // Listen for available interfaces
        let available_interfaces = Rc::new(RefCell::new(Vec::<(u32, String, u32)>::new()));
        let available_interfaces_copy = Rc::clone(&available_interfaces);

        registry.quick_assign(move |_reg, event, _data| {
            if let wayland_client::protocol::wl_registry::Event::Global {
                name,
                interface,
                version,
            } = event
            {
                (*available_interfaces_copy)
                    .borrow_mut()
                    .push((name, interface, version));
            }
        });

        if !event_queue.sync_roundtrip(&mut (), |_, _, _| {}).is_ok() {
            return Err(KeyLayoutError::WaylandError);
        }

        // Bind to wl_seat if available
        // Find wl_seat tuple

        let (seat_name, _seat_interface, seat_version) = (*available_interfaces)
            .borrow()
            .iter()
            .find(|(_name, interface, _version)| interface == "wl_seat")
            .expect("wl_seat not found in available interfaces")
            .clone();

        attached.sync();

        let wl_seat = registry.bind::<WlSeat>(seat_version, seat_name);

        let capabilities = Rc::new(RefCell::new(
            wayland_client::protocol::wl_seat::Capability::empty(),
        ));
        let capabilities_copy = Rc::clone(&capabilities);
        wl_seat.quick_assign(move |_seat, event, _data| {
            if let wayland_client::protocol::wl_seat::Event::Capabilities { capabilities } = event {
                (*capabilities_copy).borrow_mut().set(capabilities, true);
            }
        });

        event_queue
            .sync_roundtrip(&mut (), |_, _, _| {})
            .expect("Error during event queue roundtrip");

        // Check capabilities of wl_seat
        if !(*capabilities)
            .borrow()
            .contains(wayland_client::protocol::wl_seat::Capability::Keyboard)
        {
            panic!("wl_seat does not have keyboard capability");
        }

        let wl_keyboard = wl_seat.get_keyboard();

        let file_descriptor = Rc::new(RefCell::new(-1));
        let size = Rc::new(RefCell::new(0));

        let file_descriptor_copy = Rc::clone(&file_descriptor);
        let size_copy = Rc::clone(&size);

        // Get keymap from compositor

        wl_keyboard.quick_assign(
            move |_object: Main<WlKeyboard>,
                  event: wayland_client::protocol::wl_keyboard::Event,
                  _data: DispatchData<'_>| {
                if let wayland_client::protocol::wl_keyboard::Event::Keymap { format, fd, size } =
                    event
                {
                    match format {
                        KeymapFormat::XkbV1 => {
                            *file_descriptor_copy.borrow_mut() = fd;
                            *size_copy.borrow_mut() = size;
                        }
                        KeymapFormat::NoKeymap => {
                            panic!("NoKeymap format");
                        }
                        _ => {
                            panic!("Keymap Format not supported");
                        }
                    };
                }
            },
        );

        event_queue.sync_roundtrip(&mut (), |_, _, _| {}).unwrap();

        // Construct keymap from file descriptor

        let ctx = unsafe { xkb_context_new(XKB_CONTEXT_NO_FLAGS) };

        let keymap_file = unsafe { File::from_raw_fd(*file_descriptor.borrow()) };

        let map = unsafe {
            MmapOptions::new()
                .len(*size.borrow() as usize)
                .map(&keymap_file)
                .unwrap()
        };

        let keymap = unsafe {
            xkb_keymap_new_from_buffer(
                ctx,
                map.as_ptr() as *const _,
                (*size.borrow() - 1).try_into().unwrap(),
                XKB_KEYMAP_FORMAT_TEXT_v1,
                XKB_KEYMAP_COMPILE_NO_FLAGS,
            )
        };
        Ok(Self { keymap })
    }

    fn _new_x11() -> Result<Self, KeyLayoutError> {
        let (conn, _) = xcb::base::Connection::connect(None).unwrap();
        let conn = conn.into_raw_conn() as *mut c_void;
        let mut major_xkb_version_out = 0;
        let mut minor_xkb_version_out = 0;
        let mut base_event_out = 0;
        let mut base_error_out = 0;

        let _ = unsafe {
            xkb_x11_setup_xkb_extension(
                conn,
                XKB_X11_MIN_MAJOR_XKB_VERSION.try_into().unwrap(),
                XKB_X11_MIN_MINOR_XKB_VERSION.try_into().unwrap(),
                XKB_X11_SETUP_XKB_EXTENSION_NO_FLAGS,
                &mut major_xkb_version_out,
                &mut minor_xkb_version_out,
                &mut base_event_out,
                &mut base_error_out,
            )
        };

        let device_id = unsafe { xkb_x11_get_core_keyboard_device_id(conn) };

        let ctx = unsafe { xkb_context_new(XKB_CONTEXT_NO_FLAGS) };

        let keymap =
            unsafe { xkb_x11_keymap_new_from_device(ctx as *mut xkb_context, conn, device_id, 0) };

        Ok(KeyLayout { keymap })
    }

    /// Construct a KeyLayout
    /// Tries to autodetect the session type using the XDG_SESSION_TYPE environment variable
    pub fn new() -> Result<KeyLayout, KeyLayoutError> {
        match env::var("XDG_SESSION_TYPE") {
            Ok(session_type) => match session_type.as_str() {
                "wayland" => Self::_new_wayland(),
                //"x11" => Self::_new_x11(),
                _ => Err(KeyLayoutError::SessionError),
            },
            Err(_e) => Err(KeyLayoutError::SessionError),
        }
    }
}

/// Methods for KeyLayout specific to Unix-based systems
#[cfg(target_os = "linux")]
pub trait KeyLayoutExtUnix {
    /// Construct a KeyLayout explicitly using the Wayland protocol
    fn new_wayland() -> Result<KeyLayout, KeyLayoutError>;

    /// Construct a KeyLayout explicitly using the X11 protocol
    fn new_x11() -> Result<KeyLayout, KeyLayoutError>;
}

impl KeyLayoutExtUnix for KeyLayout {
    fn new_wayland() -> Result<KeyLayout, KeyLayoutError> {
        Self::_new_wayland()
    }

    fn new_x11() -> Result<KeyLayout, KeyLayoutError> {
        Self::_new_x11()
    }
}
