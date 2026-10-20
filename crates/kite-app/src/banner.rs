use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::time::{SystemTime, UNIX_EPOCH};

use glyphon::Color;
use winreg::RegKey;
use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ};

pub struct Line {
    pub text: String,
    pub spans: Vec<(std::ops::Range<usize>, Color)>,
}

const TEXT: Color = Color::rgb(0xcd, 0xd6, 0xf4);
const BORDER: Color = Color::rgb(0x58, 0x5b, 0x70);
const GREEN: Color = Color::rgb(0xa6, 0xe3, 0xa1);
const YELLOW: Color = Color::rgb(0xf9, 0xe2, 0xaf);
const BLUE: Color = Color::rgb(0x89, 0xb4, 0xfa);
const PINK: Color = Color::rgb(0xf5, 0xc2, 0xe7);

const FLAMINGO: Color = Color::rgb(0xf2, 0xcd, 0xcd);
const ROSEWATER: Color = Color::rgb(0xf5, 0xe0, 0xdc);
const TEAL: Color = Color::rgb(0x94, 0xe2, 0xd5);
const SKY: Color = Color::rgb(0x89, 0xdc, 0xeb);
const SAPPHIRE: Color = Color::rgb(0x74, 0xc7, 0xec);

const LOGO: [(&str, Color); 16] = [
    ("⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀", FLAMINGO),
    ("⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⡠⠔⠉⠀⣄⠀⠀⠀⠀⢀⣀⠤⠐⠚⠙", FLAMINGO),
    ("⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠠⠐⠁⠀⠀⠀⠀⢀⡇⠐⠉⠀⠀⠀⠀⠀⠀⠀", PINK),
    ("⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⣀⠊⠁⠀⠈⠀⠀⠀⠈⠀⠀⠀⠀⠀⠀⠀⡀⡐⡌⢆", PINK),
    ("⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⡠⠃⠀⠀⠀⠀⠠⠀⠀⠀⠀⠀⠀⠀⠀⠀⢀⠪⢨⢊⠂", ROSEWATER),
    ("⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⣠⠁⠀⡀⠀⠀⠀⢀⠠⢄⢄⢔⢖⠲⣤⢤⣄⠀⢪⢘⠌⠀", ROSEWATER),
    ("⠀⠀⠀⠀⠀⠀⠀⠀⢀⡠⠁⠠⠠⣄⠀⠐⠃⢈⠀⢢⠡⡣⠫⡅⡯⠛⠚⠹⠕⠀⠀⡘", YELLOW),
    ("⠀⠀⠀⠀⠀⠀⠀⣰⡢⠐⠈⡠⡑⠜⢛⢃⠠⠀⠨⠠⠑⢈⠁⡑⠃⠀⠀⠀⠀⠀⡔⠁", YELLOW),
    ("⠀⠀⠀⠀⠀⠀⠀⠁⠅⢌⠠⠂⠁⠀⠘⢈⠡⠂⠠⠀⠂⠀⠂⠀⠀⠀⠀⠀⠀⣈⣃⠀", GREEN),
    ("⠀⠀⠀⠀⠀⠀⠀⡠⠘⠁⠀⠀⠀⠀⠀⡂⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⣠⢒⠝⢌⠢⡑", GREEN),
    ("⠀⠀⠀⠀⠀⠄⠂⠀⠀⠀⠀⠀⣀⡼⡕⡪⢋⠕⡡⠣⡩⠪⡑⢍⠫⡫⠫⣖⢯⢦⠵⠜", TEAL),
    ("⠀⠀⠀⢀⠊⠀⠀⠀⠀⠀⠀⣠⢍⠢⡊⣔⢡⢊⠔⡑⢌⠬⢌⠢⡃⡊⡪⣷⢫⠨⡱⡀", TEAL),
    ("⠀⠀⠀⢲⠀⠀⠀⠀⠀⢀⠰⡟⡢⢣⠨⡂⡪⢐⢑⢌⠢⡑⡔⠵⠊⠂⠊⢿⠑⠕⣈⡇", SKY),
    ("⠀⠀⠀⠘⢤⠀⠀⠀⠀⠈⠀⢳⠨⠢⡑⠌⡢⢑⠰⢐⡵⠚⠁⠀⠀⠀⠀⠈⡄⠀⠈⠀", SKY),
    ("⠀⠀⠀⠀⠀⠈⠓⠠⢄⡀⡤⠀⠛⣔⡈⠢⢨⠔⠚⠁⠠⠀⠀⠀⠀⠀⠀⠀⠸⠀⠀⠀", SAPPHIRE),
    ("⠀⠀⠀⠀⠀⠀⠀⠀⠀⠸⠁⠀⠀⠈⠁⠉⠁⠀⠀⠀⠀⠠⠀⠀⠀⠀⠀⠀⠀⣇⠀⠀", SAPPHIRE),
];

const PALETTE: [u32; 16] = [
    0x45475a, 0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xbac2de, 0x585b70,
    0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xa6adc8,
];

pub fn lines() -> Vec<Line> {
    let info = info_lines();
    let width = LOGO
        .iter()
        .map(|(line, _)| line.chars().count())
        .max()
        .unwrap_or(0);
    let height = LOGO.len().max(info.len());
    let mut lines = Vec::with_capacity(height + 2);
    lines.push(Line {
        text: String::new(),
        spans: Vec::new(),
    });
    for row in 0..height {
        let mut text = String::new();
        let mut spans = Vec::new();
        if let Some((logo, color)) = LOGO.get(row) {
            let start = text.len();
            text.push_str(logo);
            spans.push((start..text.len(), *color));
        }
        if let Some(entry) = info.get(row) {
            let padding = width.saturating_sub(
                LOGO.get(row)
                    .map(|(logo, _)| logo.chars().count())
                    .unwrap_or(0),
            );
            text.push_str(&" ".repeat(padding + 3));
            let offset = text.len();
            text.push_str(&entry.text);
            spans.extend(
                entry
                    .spans
                    .iter()
                    .map(|(range, color)| (offset + range.start..offset + range.end, *color)),
            );
        }
        lines.push(Line { text, spans });
    }
    lines.push(Line {
        text: String::new(),
        spans: Vec::new(),
    });
    lines
}

struct Entry {
    key: &'static str,
    value: String,
}

struct Group {
    title: &'static str,
    color: Color,
    entries: Vec<Entry>,
}

fn info_lines() -> Vec<Line> {
    let host = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "PC".to_string());
    let cpu = registry_string(
        r"HARDWARE\DESCRIPTION\System\CentralProcessor\0",
        "ProcessorNameString",
    )
    .or_else(|| std::env::var("PROCESSOR_IDENTIFIER").ok())
    .unwrap_or_else(|| "Unknown".to_string());
    let gpu = gpu().unwrap_or_else(|| "Unknown".to_string());
    let (os, kernel) = os().unwrap_or_else(|| ("Windows".to_string(), "Unknown".to_string()));
    let bios = registry_string(r"HARDWARE\DESCRIPTION\System\BIOS", "BaseBoardManufacturer")
        .and_then(|maker| {
            registry_strings(r"HARDWARE\DESCRIPTION\System\BIOS", "BIOSVersion")
                .map(|version| format!("{maker} {version}"))
        })
        .unwrap_or_else(|| "Unknown".to_string());
    let packages = package_count();
    let groups = vec![
        Group {
            title: "Hardware",
            color: GREEN,
            entries: vec![
                Entry {
                    key: "󰌢  PC ",
                    value: host,
                },
                Entry {
                    key: "│ ├󰻠 ",
                    value: cpu,
                },
                Entry {
                    key: "│ ├󰍹 ",
                    value: gpu,
                },
                Entry {
                    key: "│ ├󰑭 ",
                    value: memory(),
                },
                Entry {
                    key: "└ └󰋊 ",
                    value: disk(),
                },
            ],
        },
        Group {
            title: "Software",
            color: YELLOW,
            entries: vec![
                Entry {
                    key: "󰄛  OS ",
                    value: os,
                },
                Entry {
                    key: "│ ├ ",
                    value: kernel,
                },
                Entry {
                    key: "│ ├󰖡 ",
                    value: bios,
                },
                Entry {
                    key: "│ ├󰏗 ",
                    value: packages.to_string(),
                },
                Entry {
                    key: "└ └󰞷 ",
                    value: format!("kite {}", env!("CARGO_PKG_VERSION")),
                },
            ],
        },
        Group {
            title: "Desktop / Window",
            color: BLUE,
            entries: vec![
                Entry {
                    key: "󰧨  DE ",
                    value: "Fluent".to_string(),
                },
                Entry {
                    key: "│ ├󰍁 ",
                    value: "Windows".to_string(),
                },
                Entry {
                    key: "│ ├󱂬 ",
                    value: "DWM".to_string(),
                },
                Entry {
                    key: "│ ├󰉦 ",
                    value: "Mica".to_string(),
                },
                Entry {
                    key: "└ └󰆍 ",
                    value: "kite".to_string(),
                },
            ],
        },
        Group {
            title: "Uptime / Age / DT",
            color: PINK,
            entries: timing_entries(),
        },
    ];

    let mut entries = Vec::new();
    for group in &groups {
        for entry in &group.entries {
            entries.push(format!("{}{}", entry.key, entry.value).chars().count());
        }
    }
    let width = entries.into_iter().max().unwrap_or(0).max(54);

    let mut lines = Vec::new();
    for group in groups {
        lines.push(box_header(group.title, width));
        for entry in group.entries {
            lines.push(styled_entry(entry.key, &entry.value, group.color));
        }
        lines.push(box_footer(width));
        lines.push(Line {
            text: String::new(),
            spans: Vec::new(),
        });
    }
    lines.push(colors());
    lines
}

fn timing_entries() -> Vec<Entry> {
    let mut entries = Vec::new();
    if let Some(days) = os_age() {
        entries.push(Entry {
            key: "  ›  OS Age  ",
            value: format!("{days} days"),
        });
    }
    entries.push(Entry {
        key: "  ›  Uptime  ",
        value: uptime(),
    });
    entries.push(Entry {
        key: "  ›  DateTime  ",
        value: datetime(),
    });
    entries
}

fn styled_entry(key: &'static str, value: &str, key_color: Color) -> Line {
    let mut text = String::new();
    let mut spans = Vec::new();
    text.push_str(key);
    spans.push((0..text.len(), key_color));
    let start = text.len();
    text.push_str(value);
    spans.push((start..text.len(), TEXT));
    Line { text, spans }
}

fn box_header(title: &str, width: usize) -> Line {
    let inner = width.saturating_sub(2);
    let dashes = inner.saturating_sub(title.chars().count());
    let left = dashes / 2;
    let text = format!(
        "┌{}{}{}┐",
        "─".repeat(left),
        title,
        "─".repeat(dashes - left)
    );
    Line {
        text: text.clone(),
        spans: vec![(0..text.len(), BORDER)],
    }
}

fn box_footer(width: usize) -> Line {
    let text = format!("└{}┘", "─".repeat(width.saturating_sub(2)));
    Line {
        text: text.clone(),
        spans: vec![(0..text.len(), BORDER)],
    }
}

fn colors() -> Line {
    let mut text = String::from("                  ");
    let mut spans = Vec::new();
    for (index, value) in PALETTE.iter().enumerate() {
        if index > 0 {
            text.push(' ');
        }
        let start = text.len();
        text.push('●');
        spans.push((
            start..text.len(),
            Color::rgb((value >> 16) as u8, (value >> 8) as u8, *value as u8),
        ));
    }
    Line { text, spans }
}

fn registry_string(path: &str, name: &str) -> Option<String> {
    let key = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(path, KEY_READ)
        .ok()?;
    let value: String = key.get_value(name).ok()?;
    let value = value.trim().to_string();
    if value.is_empty() { None } else { Some(value) }
}

fn registry_strings(path: &str, name: &str) -> Option<String> {
    let key = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(path, KEY_READ)
        .ok()?;
    let values: Vec<String> = key.get_value(name).ok()?;
    values
        .into_iter()
        .map(|value| value.trim().to_string())
        .find(|value| !value.is_empty())
}

fn gpu() -> Option<String> {
    let base = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(
            r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}",
            KEY_READ,
        )
        .ok()?;
    for index in 0..4 {
        if let Ok(key) = base.open_subkey_with_flags(format!("{index:04}"), KEY_READ) {
            if let Ok(value) = key.get_value::<String, _>("DriverDesc") {
                let value = value.trim().to_string();
                if !value.is_empty() {
                    return Some(value);
                }
            }
        }
    }
    None
}

fn os() -> Option<(String, String)> {
    let key = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion", KEY_READ)
        .ok()?;
    let mut name: String = key
        .get_value("ProductName")
        .unwrap_or_else(|_| "Windows".to_string());
    let build: String = key.get_value("CurrentBuildNumber").unwrap_or_default();
    if build.parse::<u32>().is_ok_and(|value| value >= 22_000) {
        name = name.replace("Windows 10", "Windows 11");
    }
    name = localized_edition(&name);
    if let Ok(version) = key.get_value::<String, _>("DisplayVersion") {
        if !version.trim().is_empty() {
            name = format!("{name} ({})", version.trim());
        }
    }
    name = format!("{name} {}", std::env::consts::ARCH);
    let kernel = match key.get_value::<u32, _>("UBR") {
        Ok(ubr) if !build.is_empty() => format!("10.0.{build}.{ubr}"),
        _ if !build.is_empty() => format!("10.0.{build}"),
        _ => "Unknown".to_string(),
    };
    Some((name, kernel))
}

fn localized_edition(name: &str) -> String {
    for (suffix, localized) in [
        ("Home China", "家庭中文版"),
        ("Home", "家庭版"),
        ("Pro", "专业版"),
        ("Enterprise", "企业版"),
        ("Education", "教育版"),
    ] {
        if let Some(prefix) = name.strip_suffix(suffix) {
            return format!("{prefix}{localized}");
        }
    }
    name.to_string()
}

fn os_age() -> Option<u64> {
    let key = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion", KEY_READ)
        .ok()?;
    let installed: u32 = key.get_value("InstallDate").ok()?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some(now.saturating_sub(installed as u64) / 86_400)
}

fn package_count() -> usize {
    let mut count = 0;
    for path in [
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
        r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
    ] {
        let Ok(key) = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey_with_flags(path, KEY_READ)
        else {
            continue;
        };
        for name in key.enum_keys().flatten() {
            if let Ok(entry) = key.open_subkey(name) {
                if entry.get_value::<String, _>("DisplayName").is_ok() {
                    count += 1;
                }
            }
        }
    }
    count
}

#[repr(C)]
struct MemoryStatus {
    length: u32,
    memory_load: u32,
    total_physical: u64,
    available_physical: u64,
    total_page_file: u64,
    available_page_file: u64,
    total_virtual: u64,
    available_virtual: u64,
    available_extended_virtual: u64,
}

#[repr(C)]
struct LocalTime {
    year: u16,
    month: u16,
    day_of_week: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    milliseconds: u16,
}

unsafe extern "system" {
    fn GlobalMemoryStatusEx(buffer: *mut MemoryStatus) -> i32;
    fn GetDiskFreeSpaceExW(
        directory: *const u16,
        free_bytes_available: *mut u64,
        total_bytes: *mut u64,
        total_free_bytes: *mut u64,
    ) -> i32;
    fn GetLocalTime(time: *mut LocalTime);
    fn GetTickCount64() -> u64;
}

fn memory() -> String {
    let mut status: MemoryStatus = unsafe { std::mem::zeroed() };
    status.length = std::mem::size_of::<MemoryStatus>() as u32;
    if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 {
        return "Unknown".to_string();
    }
    let used = status
        .total_physical
        .saturating_sub(status.available_physical);
    format!("{} / {}", gigabytes(used), gigabytes(status.total_physical))
}

fn disk() -> String {
    let path: Vec<u16> = OsStr::new("C:\\")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut free = 0u64;
    let mut total = 0u64;
    let mut total_free = 0u64;
    if unsafe { GetDiskFreeSpaceExW(path.as_ptr(), &mut free, &mut total, &mut total_free) } == 0 {
        return "Unknown".to_string();
    }
    format!(
        "C: {} / {}",
        gigabytes(total.saturating_sub(total_free)),
        gigabytes(total)
    )
}

fn gigabytes(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / 1_073_741_824.0)
}

fn uptime() -> String {
    let seconds = unsafe { GetTickCount64() } / 1000;
    let days = seconds / 86_400;
    let hours = seconds % 86_400 / 3_600;
    let minutes = seconds % 3_600 / 60;
    if days > 0 {
        format!("{days}d {hours}h {minutes}m")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

fn datetime() -> String {
    let mut time: LocalTime = unsafe { std::mem::zeroed() };
    unsafe { GetLocalTime(&mut time) };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        time.year, time.month, time.day, time.hour, time.minute, time.second
    )
}
