//! Human-readable tables for `show` commands, built from the JSON that
//! `ubus call` prints. Pure functions: JSON text in, display text out.
//! Anything unexpected makes a renderer return `None`, and the caller falls
//! back to the raw output instead of hiding information.

use super::json::{parse, Value};

/// Left-aligned columns separated by two spaces; no trailing blanks. The
/// header is underlined with dashes.
pub fn table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let cols = headers.len();
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for r in rows {
        for (i, cell) in r.iter().enumerate().take(cols) {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }
    let line = |cells: &[&str]| -> String {
        let mut s = String::new();
        for (i, c) in cells.iter().enumerate() {
            s.push_str(c);
            if i + 1 < cols {
                s.push_str(&" ".repeat(widths[i] - c.chars().count() + 2));
            }
        }
        format!("{}\n", s.trim_end())
    };
    let dashes: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
    let mut out = line(headers);
    out.push_str(&line(
        &dashes.iter().map(String::as_str).collect::<Vec<_>>(),
    ));
    for r in rows {
        let cells: Vec<&str> = (0..cols)
            .map(|i| r.get(i).map(String::as_str).unwrap_or(""))
            .collect();
        out.push_str(&line(&cells));
    }
    out
}

/// `3d 02:11:05`, or `02:11:05` below one day.
pub fn uptime(secs: u64) -> String {
    let (d, h, m, s) = (
        secs / 86_400,
        secs % 86_400 / 3600,
        secs % 3600 / 60,
        secs % 60,
    );
    if d > 0 {
        format!("{d}d {h:02}:{m:02}:{s:02}")
    } else {
        format!("{h:02}:{m:02}:{s:02}")
    }
}

fn mib(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}

fn addresses(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_arr)
        .unwrap_or(&[])
        .iter()
        .filter_map(|a| {
            let addr = a.get("address")?.as_str()?;
            Some(match a.get("mask").and_then(Value::as_u64) {
                Some(m) => format!("{addr}/{m}"),
                None => addr.to_string(),
            })
        })
        .collect()
}

fn join_or_dash(v: Vec<String>) -> String {
    if v.is_empty() {
        "-".to_string()
    } else {
        v.join(", ")
    }
}

/// `ubus call network.interface dump` -> table of interfaces.
pub fn interfaces(json: &str) -> Option<String> {
    let root = parse(json)?;
    let list = root.get("interface")?.as_arr()?;
    let mut rows = Vec::new();
    let mut any_v6 = false;
    for i in list {
        let name = i.get("interface")?.as_str()?.to_string();
        let up = i.get("up").and_then(Value::as_bool).unwrap_or(false);
        let device = i
            .get("l3_device")
            .or_else(|| i.get("device"))
            .and_then(Value::as_str)
            .unwrap_or("-")
            .to_string();
        let v6 = addresses(i.get("ipv6-address"));
        any_v6 |= !v6.is_empty();
        rows.push(vec![
            name,
            i.get("proto")
                .and_then(Value::as_str)
                .unwrap_or("-")
                .to_string(),
            device,
            if up { "up" } else { "down" }.to_string(),
            match (up, i.get("uptime").and_then(Value::as_u64)) {
                (true, Some(s)) => uptime(s),
                _ => "-".to_string(),
            },
            join_or_dash(addresses(i.get("ipv4-address"))),
            join_or_dash(v6),
        ]);
    }
    if rows.is_empty() {
        return Some("No interfaces\n".to_string());
    }
    if any_v6 {
        Some(table(
            &[
                "Interface",
                "Proto",
                "Device",
                "Status",
                "Uptime",
                "IPv4 address",
                "IPv6 address",
            ],
            &rows,
        ))
    } else {
        let rows: Vec<Vec<String>> = rows
            .into_iter()
            .map(|mut r| {
                r.pop();
                r
            })
            .collect();
        Some(table(
            &[
                "Interface",
                "Proto",
                "Device",
                "Status",
                "Uptime",
                "IPv4 address",
            ],
            &rows,
        ))
    }
}

/// `ubus call system board` (+ optional `ubus call system info`) -> key/value
/// lines. A missing or unparsable `info` only drops the lines it would give.
pub fn system(board: &str, info: Option<&str>) -> Option<String> {
    let b = parse(board)?;
    if !matches!(b, Value::Obj(_)) {
        return None;
    }
    let s = |key: &str| b.get(key).and_then(Value::as_str).map(String::from);
    let mut lines: Vec<(&str, String)> = Vec::new();
    if let Some(v) = s("hostname") {
        lines.push(("Hostname", v));
    }
    if let Some(v) = s("model") {
        lines.push(("Model", v));
    }
    if let Some(v) = s("board_name") {
        lines.push(("Board", v));
    }
    if let Some(v) = s("system") {
        lines.push(("CPU", v));
    }
    if let Some(rel) = b.get("release") {
        let r = |key: &str| rel.get(key).and_then(Value::as_str);
        let text = match (r("description"), r("distribution"), r("version")) {
            (Some(d), _, _) => Some(d.to_string()),
            (None, Some(d), Some(v)) => Some(format!("{d} {v}")),
            _ => None,
        };
        if let Some(mut t) = text {
            if let Some(target) = r("target") {
                t.push_str(&format!(" ({target})"));
            }
            lines.push(("Release", t));
        }
    }
    if let Some(v) = s("kernel") {
        lines.push(("Kernel", v));
    }
    if let Some(v) = s("rootfs_type") {
        lines.push(("Rootfs", v));
    }
    if let Some(i) = info.and_then(parse) {
        if let Some(u) = i.get("uptime").and_then(Value::as_u64) {
            lines.push(("Uptime", uptime(u)));
        }
        if let Some(l) = i.get("load").and_then(Value::as_arr) {
            let vals: Vec<String> = l
                .iter()
                .filter_map(Value::as_f64)
                .map(|n| format!("{:.2}", n / 65536.0))
                .collect();
            if vals.len() == 3 {
                lines.push(("Load average", vals.join(" ")));
            }
        }
        if let Some(m) = i.get("memory") {
            let get = |k: &str| m.get(k).and_then(Value::as_u64);
            if let (Some(total), Some(free)) = (get("total"), get("free")) {
                let mut t = format!("{} total, {} free", mib(total), mib(free));
                if let Some(av) = get("available") {
                    t.push_str(&format!(", {} available", mib(av)));
                }
                lines.push(("Memory", t));
            }
        }
    }
    if lines.is_empty() {
        return None;
    }
    let w = lines.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
    let mut out = String::new();
    for (k, v) in lines {
        out.push_str(&format!("{k:<w$}  {v}\n"));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_aligns_columns_and_underlines_headers() {
        let t = table(
            &["Name", "Up"],
            &[
                vec!["lan".to_string(), "yes".to_string()],
                vec!["wan6".to_string(), "no".to_string()],
            ],
        );
        assert_eq!(t, "Name  Up\n----  ---\nlan   yes\nwan6  no\n");
    }

    #[test]
    fn table_widths_follow_characters_not_bytes() {
        let t = table(&["A", "B"], &[vec!["Ü€".to_string(), "x".to_string()]]);
        assert_eq!(t, "A   B\n--  -\nÜ€  x\n");
    }

    #[test]
    fn table_with_no_rows_still_has_a_header() {
        assert_eq!(table(&["A", "B"], &[]), "A  B\n-  -\n");
    }

    #[test]
    fn table_tolerates_short_rows() {
        let t = table(&["A", "B"], &[vec!["1".to_string()]]);
        assert_eq!(t, "A  B\n-  -\n1\n");
    }

    #[test]
    fn uptime_formatting() {
        assert_eq!(uptime(0), "00:00:00");
        assert_eq!(uptime(3725), "01:02:05");
        assert_eq!(uptime(86_400), "1d 00:00:00");
        assert_eq!(uptime(3 * 86_400 + 2 * 3600 + 11 * 60 + 5), "3d 02:11:05");
    }

    const IFACES: &str = r#"{
        "interface": [
            { "interface": "lan", "up": true, "uptime": 725, "proto": "static",
              "l3_device": "br-lan", "device": "br-lan",
              "ipv4-address": [ { "address": "192.168.1.1", "mask": 24 } ],
              "ipv6-address": [], "route": [] },
            { "interface": "loopback", "up": true, "uptime": 90000, "proto": "static",
              "l3_device": "lo",
              "ipv4-address": [ { "address": "127.0.0.1", "mask": 8 } ] },
            { "interface": "wan", "up": false, "proto": "dhcp", "device": "eth1",
              "ipv4-address": [], "inactive": { "ipv4-address": [] } }
        ]
    }"#;

    #[test]
    fn interfaces_table() {
        let t = interfaces(IFACES).unwrap();
        assert_eq!(
            t,
            "\
Interface  Proto   Device  Status  Uptime       IPv4 address
---------  ------  ------  ------  -----------  --------------
lan        static  br-lan  up      00:12:05     192.168.1.1/24
loopback   static  lo      up      1d 01:00:00  127.0.0.1/8
wan        dhcp    eth1    down    -            -
"
        );
    }

    #[test]
    fn interfaces_adds_an_ipv6_column_only_when_needed() {
        let with_v6 = r#"{"interface":[{"interface":"lan","up":true,"uptime":1,"proto":"static",
            "l3_device":"br-lan","ipv4-address":[],
            "ipv6-address":[{"address":"fd00::1","mask":64},{"address":"fe80::1","mask":64}]}]}"#;
        let t = interfaces(with_v6).unwrap();
        assert!(t.lines().next().unwrap().ends_with("IPv6 address"));
        assert!(t.contains("fd00::1/64, fe80::1/64"));
        assert!(!interfaces(IFACES).unwrap().contains("IPv6"));
    }

    #[test]
    fn interfaces_without_any_entries() {
        assert_eq!(
            interfaces(r#"{"interface": []}"#).unwrap(),
            "No interfaces\n"
        );
    }

    #[test]
    fn interfaces_gives_up_on_unexpected_shapes() {
        for bad in [
            "",
            "not json",
            "[]",
            "{}",
            r#"{"interface": {}}"#,
            r#"{"interface":[{"up":true}]}"#,
        ] {
            assert_eq!(interfaces(bad), None, "{bad:?}");
        }
    }

    const BOARD: &str = r#"{
        "kernel": "6.12.67", "hostname": "OpenWrt", "system": "Intel(R) Xeon(R)",
        "model": "QEMU Standard PC (i440FX + PIIX, 1996)",
        "board_name": "qemu-standard-pc-i440fx-piix-1996", "rootfs_type": "ext4",
        "release": { "distribution": "OpenWrt", "version": "25.12.5",
                     "revision": "r12345", "target": "x86/64",
                     "description": "OpenWrt 25.12.5 r12345" }
    }"#;
    const INFO: &str = r#"{
        "uptime": 90061, "localtime": 1700000000,
        "load": [65536, 32768, 0],
        "memory": { "total": 262144000, "free": 209715200, "available": 230000000 }
    }"#;

    #[test]
    fn system_page() {
        let t = system(BOARD, Some(INFO)).unwrap();
        assert_eq!(
            t,
            "\
Hostname      OpenWrt
Model         QEMU Standard PC (i440FX + PIIX, 1996)
Board         qemu-standard-pc-i440fx-piix-1996
CPU           Intel(R) Xeon(R)
Release       OpenWrt 25.12.5 r12345 (x86/64)
Kernel        6.12.67
Rootfs        ext4
Uptime        1d 01:01:01
Load average  1.00 0.50 0.00
Memory        250.0 MiB total, 200.0 MiB free, 219.3 MiB available
"
        );
    }

    #[test]
    fn system_without_info_still_shows_the_board() {
        let t = system(BOARD, None).unwrap();
        assert!(t.contains("Hostname  OpenWrt"));
        assert!(!t.contains("Uptime"));
        let t = system(BOARD, Some("garbage")).unwrap();
        assert!(!t.contains("Uptime"));
    }

    #[test]
    fn system_builds_the_release_from_parts_when_there_is_no_description() {
        let b = r#"{"hostname":"r","release":{"distribution":"OpenWrt","version":"25.12.5"}}"#;
        assert!(system(b, None)
            .unwrap()
            .contains("Release   OpenWrt 25.12.5\n"));
    }

    #[test]
    fn system_gives_up_on_unexpected_shapes() {
        for bad in ["", "[]", "{}", "{\"unrelated\": 1}"] {
            assert_eq!(system(bad, None), None, "{bad:?}");
        }
    }
}
