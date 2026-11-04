//! Identifies input devices from their Windows device paths.
//!
//! Raw Input device names and Interception hardware ids carry the same
//! identifiers, so this logic stays free of Windows API calls.

pub const LOGITECH_VID: u16 = 0x046D;
/// MX Ergo product id when paired directly over Bluetooth.
pub const MX_ERGO_BT_PID: u16 = 0xB01D;

const LOGITECH_RECEIVER_PIDS: [u16; 3] = [0xC52B, 0xC532, 0xC548];

/// USB and Bluetooth HID service UUIDs as they appear in device paths.
const BLE_HID_SERVICE: &str = "{00001812";
const BT_CLASSIC_HID_SERVICE: &str = "{00001124";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Connection {
    Bluetooth,
    LogitechReceiver,
    Other,
}

/// Extracts `(vid, pid)` from a device path.
///
/// Handles both `VID_046D&PID_C52B` (USB) and `VID&02046d_PID&b01d`
/// (Bluetooth, where the VID is prefixed with its vendor id source).
pub fn parse_vid_pid(path: &str) -> Option<(u16, u16)> {
    let upper = path.to_ascii_uppercase();
    let vid = id_after(&upper, "VID_").or_else(|| id_after(&upper, "VID&"))?;
    let pid = id_after(&upper, "PID_").or_else(|| id_after(&upper, "PID&"))?;
    Some((vid, pid))
}

/// Reads the hex run after `key` and takes its last four digits.
fn id_after(text: &str, key: &str) -> Option<u16> {
    let start = text.find(key)? + key.len();
    let rest = &text[start..];
    let end = rest
        .find(|c: char| !c.is_ascii_hexdigit())
        .unwrap_or(rest.len());
    let digits = &rest[..end];
    if digits.len() < 4 {
        return None;
    }
    u16::from_str_radix(&digits[digits.len() - 4..], 16).ok()
}

pub fn classify_connection(path: &str) -> Connection {
    let upper = path.to_ascii_uppercase();
    if upper.contains(BLE_HID_SERVICE) || upper.contains(BT_CLASSIC_HID_SERVICE) {
        return Connection::Bluetooth;
    }
    match parse_vid_pid(path) {
        Some((LOGITECH_VID, pid)) if LOGITECH_RECEIVER_PIDS.contains(&pid) => {
            Connection::LogitechReceiver
        }
        _ => Connection::Other,
    }
}

/// Whether a device is positively an MX Ergo.
pub fn is_mx_ergo(path: &str, product: Option<&str>) -> bool {
    let named_mx_ergo =
        product.is_some_and(|name| name.to_ascii_lowercase().contains("mx ergo"));
    match parse_vid_pid(path) {
        Some((LOGITECH_VID, pid)) => pid == MX_ERGO_BT_PID || named_mx_ergo,
        _ => false,
    }
}

/// A name for a device that stays the same across reconnects and reboots, and
/// reads the same from a Raw Input path and an Interception hardware id.
pub fn device_key(path: &str) -> String {
    let upper = path.to_ascii_uppercase().replace('\\', "#");
    let trimmed = upper.trim_start_matches(['#', '?']);
    let mut parts = trimmed.split('#').filter(|part| !part.is_empty());
    match (parts.next(), parts.next()) {
        (Some("HID"), Some(name)) => name.to_string(),
        (Some(first), Some(second)) => format!("{first}#{second}"),
        (Some(first), None) => first.to_string(),
        _ => upper.clone(),
    }
}

/// Whether a device is the one to filter. `device_match`, when set, is a piece
/// of the device path that replaces the automatic choice. That is needed behind
/// a receiver, whose path does not tell which mouse it serves.
pub fn is_target(path: &str, product: Option<&str>, device_match: Option<&str>) -> bool {
    match device_match {
        Some(piece) => path
            .to_ascii_uppercase()
            .contains(&piece.to_ascii_uppercase()),
        None => is_mx_ergo(path, product),
    }
}

/// How well a device matches the MX Ergo: 0 no match, 1 any Logitech mouse,
/// 2 positively an MX Ergo.
pub fn mx_ergo_rank(path: &str, product: Option<&str>) -> u8 {
    if is_mx_ergo(path, product) {
        return 2;
    }
    match parse_vid_pid(path) {
        Some((LOGITECH_VID, _)) => 1,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECEIVER: &str = r"\\?\HID#VID_046D&PID_C52B&MI_01&Col01#8&1a2b3c&0&0000#{378de44c}";
    const BLE: &str = r"\\?\HID#{00001812-0000-1000-8000-00805f9b34fb}_Dev_VID&02046d_PID&b01d_REV&0005_abcdef#9&1&0&0000#{378de44c}";
    const BT_CLASSIC: &str = r"\\?\HID#{00001124-0000-1000-8000-00805f9b34fb}_VID&0002046d_PID&b01d#8&2&0&0000#{378de44c}";
    const OTHER: &str = r"\\?\HID#VID_1532&PID_0084&MI_00#7&3&0&0000#{378de44c}";
    const RDP: &str = r"\\?\Root#RDP_MOU#0000#{378de44c}";

    #[test]
    fn parses_usb_paths() {
        assert_eq!(parse_vid_pid(RECEIVER), Some((0x046D, 0xC52B)));
    }

    #[test]
    fn parses_bluetooth_paths() {
        assert_eq!(parse_vid_pid(BLE), Some((0x046D, 0xB01D)));
        assert_eq!(parse_vid_pid(BT_CLASSIC), Some((0x046D, 0xB01D)));
    }

    #[test]
    fn paths_without_ids_parse_to_none() {
        assert_eq!(parse_vid_pid(RDP), None);
    }

    #[test]
    fn classifies_connections() {
        assert_eq!(classify_connection(RECEIVER), Connection::LogitechReceiver);
        assert_eq!(classify_connection(BLE), Connection::Bluetooth);
        assert_eq!(classify_connection(BT_CLASSIC), Connection::Bluetooth);
        assert_eq!(classify_connection(OTHER), Connection::Other);
    }

    #[test]
    fn device_keys_drop_what_changes_between_connections() {
        assert_eq!(device_key(RECEIVER), "VID_046D&PID_C52B&MI_01&COL01");
        assert_eq!(
            device_key(BLE),
            "{00001812-0000-1000-8000-00805F9B34FB}_DEV_VID&02046D_PID&B01D_REV&0005_ABCDEF"
        );
    }

    #[test]
    fn raw_input_paths_and_hardware_ids_give_the_same_key() {
        let hardware_id = r"HID\VID_046D&PID_C52B&MI_01&Col01";
        assert_eq!(device_key(hardware_id), device_key(RECEIVER));
    }

    #[test]
    fn device_keys_work_for_devices_outside_hid() {
        assert_eq!(device_key(RDP), "ROOT#RDP_MOU");
    }

    #[test]
    fn only_the_mx_ergo_is_a_target_by_default() {
        assert!(is_target(BLE, None, None));
        assert!(!is_target(RECEIVER, Some("USB Receiver"), None));
        assert!(!is_target(OTHER, None, None));
    }

    #[test]
    fn device_match_replaces_the_automatic_choice() {
        assert!(is_target(RECEIVER, None, Some("pid_c52b")));
        assert!(!is_target(BLE, None, Some("pid_c52b")));
    }

    #[test]
    fn ranks_the_mx_ergo_above_other_mice() {
        assert_eq!(mx_ergo_rank(BLE, None), 2);
        assert_eq!(mx_ergo_rank(RECEIVER, Some("MX Ergo")), 2);
        assert_eq!(mx_ergo_rank(RECEIVER, Some("USB Receiver")), 1);
        assert_eq!(mx_ergo_rank(OTHER, Some("MX Ergo")), 0);
        assert_eq!(mx_ergo_rank(RDP, None), 0);
    }
}
