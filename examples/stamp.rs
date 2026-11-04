//! Build tool: writes the icon and version info into a built exe.
//!
//! The GNU toolchain has no resource compiler, so this uses the Win32 resource-update API instead.
//!
//! Usage: `stamp <exe> <ico>`

use std::path::Path;
use windows_sys::Win32::System::LibraryLoader::{
    BeginUpdateResourceW, EndUpdateResourceW, UpdateResourceW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{RT_GROUP_ICON, RT_ICON, RT_VERSION};

const LANGUAGE_EN_US: u16 = 0x0409;
const CODEPAGE_UNICODE: u16 = 0x04B0;
const ICON_GROUP_ID: u16 = 1;
const VERSION_ID: u16 = 1;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn u16_at(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

fn u32_at(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())
}

/// Splits an .ico file into a group-icon directory and its images, as resources store them.
fn parse_icon(ico: &[u8]) -> (Vec<u8>, Vec<&[u8]>) {
    let count = u16_at(ico, 4) as usize;
    let mut group = ico[..6].to_vec();
    let mut images = Vec::with_capacity(count);
    for i in 0..count {
        let entry = &ico[6 + i * 16..6 + (i + 1) * 16];
        let size = u32_at(entry, 8) as usize;
        let offset = u32_at(entry, 12) as usize;
        images.push(&ico[offset..offset + size]);
        // A group entry matches a file entry, except that a resource id replaces the file offset.
        group.extend_from_slice(&entry[..12]);
        group.extend_from_slice(&(i as u16 + 1).to_le_bytes());
    }
    (group, images)
}

fn pad_to_u32(out: &mut Vec<u8>) {
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}

/// Encodes one node of a version resource tree.
fn version_block(
    key: &str,
    value: &[u8],
    value_units: u16,
    is_text: bool,
    children: &[Vec<u8>],
) -> Vec<u8> {
    let mut block = vec![0, 0]; // Total length, filled in below.
    block.extend_from_slice(&value_units.to_le_bytes());
    block.extend_from_slice(&u16::from(is_text).to_le_bytes());
    for unit in wide(key) {
        block.extend_from_slice(&unit.to_le_bytes());
    }
    pad_to_u32(&mut block);
    block.extend_from_slice(value);
    for child in children {
        pad_to_u32(&mut block);
        block.extend_from_slice(child);
    }
    let length = block.len() as u16;
    block[..2].copy_from_slice(&length.to_le_bytes());
    block
}

fn version_string(key: &str, value: &str) -> Vec<u8> {
    let units = wide(value);
    let bytes: Vec<u8> = units.iter().flat_map(|unit| unit.to_le_bytes()).collect();
    version_block(key, &bytes, units.len() as u16, true, &[])
}

fn version_resource() -> Vec<u8> {
    let version = env!("CARGO_PKG_VERSION");
    let part = |name: &str| -> u32 { name.parse().expect("version parts must be numeric") };
    let (major, minor, patch) = (
        part(env!("CARGO_PKG_VERSION_MAJOR")),
        part(env!("CARGO_PKG_VERSION_MINOR")),
        part(env!("CARGO_PKG_VERSION_PATCH")),
    );

    const VOS_NT_WINDOWS32: u32 = 0x0004_0004;
    const VFT_APP: u32 = 1;
    let fixed_fields: [u32; 13] = [
        0xFEEF_04BD, // Signature
        0x0001_0000, // Structure version
        (major << 16) | minor,
        patch << 16,
        (major << 16) | minor,
        patch << 16,
        0x3F, // Which file flags are meaningful
        0,    // File flags
        VOS_NT_WINDOWS32,
        VFT_APP,
        0,
        0,
        0,
    ];
    let fixed: Vec<u8> = fixed_fields.iter().flat_map(|f| f.to_le_bytes()).collect();

    let table_name = format!("{LANGUAGE_EN_US:04X}{CODEPAGE_UNICODE:04X}");
    let strings = [
        version_string("FileDescription", "Sound Honter"),
        version_string("FileVersion", version),
        version_string("InternalName", "soundhonter"),
        version_string("OriginalFilename", "soundhonter.exe"),
        version_string("ProductName", "Sound Honter"),
        version_string("ProductVersion", version),
    ];
    let table = version_block(&table_name, &[], 0, true, &strings);
    let string_info = version_block("StringFileInfo", &[], 0, true, &[table]);

    let translation: Vec<u8> = [LANGUAGE_EN_US, CODEPAGE_UNICODE]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    let var = version_block(
        "Translation",
        &translation,
        translation.len() as u16,
        false,
        &[],
    );
    let var_info = version_block("VarFileInfo", &[], 0, true, &[var]);

    version_block(
        "VS_VERSION_INFO",
        &fixed,
        fixed.len() as u16,
        false,
        &[string_info, var_info],
    )
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [exe, ico] = args.as_slice() else {
        panic!("usage: stamp <exe> <ico>");
    };
    assert!(Path::new(exe).is_file(), "{exe} doesn't exist");
    let ico = std::fs::read(ico).expect("couldn't read the icon");
    let (group, images) = parse_icon(&ico);
    let version = version_resource();

    // SAFETY: the buffers outlive each call, and the API copies what it's given.
    unsafe {
        let exe = wide(exe);
        let update = BeginUpdateResourceW(exe.as_ptr(), 0);
        assert!(!update.is_null(), "couldn't open the exe for updating");
        let write = |kind: *const u16, id: u16, data: &[u8]| {
            let ok = UpdateResourceW(
                update,
                kind,
                id as usize as *const u16,
                LANGUAGE_EN_US,
                data.as_ptr() as *const _,
                data.len() as u32,
            );
            assert!(ok != 0, "couldn't write resource {id}");
        };
        for (i, image) in images.iter().enumerate() {
            write(RT_ICON, i as u16 + 1, image);
        }
        write(RT_GROUP_ICON, ICON_GROUP_ID, &group);
        write(RT_VERSION, VERSION_ID, &version);
        assert!(EndUpdateResourceW(update, 0) != 0, "couldn't save the exe");
    }
}
