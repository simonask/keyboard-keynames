#![warn(missing_docs)]

//! keynames is a crate to convert keyboard scan codes to OS-defined key strings

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}

#[cfg(target_os = "windows")]
#[path = "platform/mod.rs"]
pub mod key_layout;

#[cfg(target_os = "linux")]
#[path = "platform/mod.rs"]
pub mod key_layout;
