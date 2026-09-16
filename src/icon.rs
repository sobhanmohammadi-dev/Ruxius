//! `rux build --icon <path.ico>` sets the built app's icon — without
//! recompiling, same as everything else in Ruxius. That means patching
//! resources into an *already-built* PE executable rather than the usual
//! `build.rs`-time resource compilation most Rust tooling uses (`winres`,
//! `embed-resource`), since there's nothing being compiled here.
//!
//! [`winres-edit`](https://docs.rs/winres-edit) wraps the real Win32
//! mechanism for this (`BeginUpdateResource`/`UpdateResource`/
//! `EndUpdateResource` — the same API `rcedit` and Resource Hacker use to
//! edit an existing `.exe`), so the actual resource-table surgery isn't
//! something this module reimplements. What it does do itself: parse the
//! `.ico` file and build the `GRPICONDIR` structure Windows expects,
//! since that's just a well-defined byte format, not a Win32 API call.
//!
//! Icon embedding happens on a copy of the *stub* (before the payload is
//! appended), so any resource-section resizing `UpdateResource` does stays
//! well clear of the self-appending format's trailing footer.

use crate::error::{LauncherError, Result};
use std::path::Path;

/// One image inside a `.ico` file.
struct IcoImage {
    width: u8,
    height: u8,
    color_count: u8,
    planes: u16,
    bit_count: u16,
    data: Vec<u8>,
}

/// Parses a `.ico` file's `ICONDIR` header, `ICONDIRENTRY` table, and each
/// image's raw bytes. This is plain byte-format parsing — no Windows API
/// involved — so it's compiled and available on every platform, even
/// though actually *using* the result to patch an executable is
/// Windows-only.
fn parse_ico(bytes: &[u8]) -> Result<Vec<IcoImage>> {
    const HEADER_LEN: usize = 6;
    const ENTRY_LEN: usize = 16;

    let bad = |msg: &str| LauncherError::Extraction(format!("not a valid .ico file: {msg}"));

    if bytes.len() < HEADER_LEN {
        return Err(bad("file too short"));
    }
    let reserved = u16::from_le_bytes([bytes[0], bytes[1]]);
    let kind = u16::from_le_bytes([bytes[2], bytes[3]]);
    let count = u16::from_le_bytes([bytes[4], bytes[5]]) as usize;
    if reserved != 0 || kind != 1 {
        return Err(bad("missing ICO header (reserved=0, type=1)"));
    }
    if count == 0 {
        return Err(bad("contains no images"));
    }

    let table_end = HEADER_LEN + count * ENTRY_LEN;
    if bytes.len() < table_end {
        return Err(bad("truncated directory table"));
    }

    let mut images = Vec::with_capacity(count);
    for i in 0..count {
        let e = &bytes[HEADER_LEN + i * ENTRY_LEN..HEADER_LEN + (i + 1) * ENTRY_LEN];
        let width = e[0];
        let height = e[1];
        let color_count = e[2];
        let planes = u16::from_le_bytes([e[4], e[5]]);
        let bit_count = u16::from_le_bytes([e[6], e[7]]);
        let size = u32::from_le_bytes([e[8], e[9], e[10], e[11]]) as usize;
        let offset = u32::from_le_bytes([e[12], e[13], e[14], e[15]]) as usize;

        let end = offset
            .checked_add(size)
            .ok_or_else(|| bad("image entry overflows"))?;
        if end > bytes.len() {
            return Err(bad("image entry points past end of file"));
        }

        images.push(IcoImage {
            width,
            height,
            color_count,
            planes,
            bit_count,
            data: bytes[offset..end].to_vec(),
        });
    }

    Ok(images)
}

/// Builds the `GRPICONDIR` + `GRPICONDIRENTRY[]` blob for the
/// `RT_GROUP_ICON` resource: the same shape as a `.ico` file's own header
/// and directory table, except each entry references an `RT_ICON`
/// resource ID instead of a byte offset into the file. This is what
/// Explorer/the taskbar actually look up to render the exe's icon.
fn build_group_icon_data(images: &[IcoImage], ids: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(6 + images.len() * 14);
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    out.extend_from_slice(&1u16.to_le_bytes()); // type = icon
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());

    for (image, &id) in images.iter().zip(ids) {
        out.push(image.width);
        out.push(image.height);
        out.push(image.color_count);
        out.push(0); // reserved
        out.extend_from_slice(&image.planes.to_le_bytes());
        out.extend_from_slice(&image.bit_count.to_le_bytes());
        out.extend_from_slice(&(image.data.len() as u32).to_le_bytes());
        out.extend_from_slice(&id.to_le_bytes());
    }

    out
}

/// Copies `base_exe` to `output`, then embeds `ico_path`'s icon into that
/// copy's PE resources. `output` is meant to be a temp file that becomes
/// the stub `payload::build_output` appends the payload to — never the
/// user's final output path directly, so a failure here can't leave a
/// half-modified file at the path they asked for.
#[cfg(windows)]
pub fn embed_icon(base_exe: &Path, ico_path: &Path, output: &Path) -> Result<()> {
    use winres_edit::{Id, Resource, Resources, resource_type};

    let ico_bytes = std::fs::read(ico_path)
        .map_err(|e| LauncherError::Extraction(format!("reading {}: {e}", ico_path.display())))?;
    let images = parse_ico(&ico_bytes)?;

    std::fs::copy(base_exe, output)
        .map_err(|e| LauncherError::Extraction(format!("copying stub for icon embedding: {e}")))?;

    // RT_ICON resource IDs are arbitrary as long as they're unique within
    // the file; a freshly built Ruxius stub has none of its own, so a
    // simple 101, 102, ... sequence can't collide with anything.
    let ids: Vec<u16> = (0..images.len() as u16).map(|i| 101 + i).collect();
    let group_data = build_group_icon_data(&images, &ids);

    let to_io_err = |e: winres_edit::Error| {
        LauncherError::Extraction(format!("embedding icon into {}: {e}", output.display()))
    };

    let mut resources = Resources::new(output);
    resources.open().map_err(to_io_err)?;

    for (image, &id) in images.iter().zip(&ids) {
        Resource::new(&resources, resource_type::ICON.into(), Id::Integer(id).into(), 0, &image.data)
            .update()
            .map_err(to_io_err)?;
    }
    // The crate's `resource_type` module doesn't define a GROUP_ICON
    // constant (checked its source directly), so this uses the raw
    // RT_GROUP_ICON value — 14, per the Win32 resource-type enumeration,
    // stable and documented since Windows 3.0.
    const RT_GROUP_ICON: u16 = 14;
    Resource::new(&resources, Id::Integer(RT_GROUP_ICON).into(), Id::Integer(1).into(), 0, &group_data)
        .update()
        .map_err(to_io_err)?;

    resources.close();
    Ok(())
}

#[cfg(not(windows))]
pub fn embed_icon(_base_exe: &Path, _ico_path: &Path, _output: &Path) -> Result<()> {
    Err(LauncherError::Extraction(
        "--icon embeds a Windows PE resource and is only supported when building on Windows"
            .into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a minimal but well-formed single-image `.ico` file, by hand,
    /// straight from the format spec — not by calling any code from this
    /// module — so this is a genuine independent check of `parse_ico`,
    /// not a tautology that would pass even if the parser had the wrong
    /// field offsets.
    fn sample_ico_bytes() -> Vec<u8> {
        let image_data: Vec<u8> = vec![0xAA, 0xBB, 0xCC, 0xDD, 0x11, 0x22, 0x33, 0x44];
        let offset: u32 = 6 + 16; // ICONDIR header + one ICONDIRENTRY

        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0u16.to_le_bytes()); // reserved
        bytes.extend_from_slice(&1u16.to_le_bytes()); // type = icon
        bytes.extend_from_slice(&1u16.to_le_bytes()); // count = 1

        bytes.push(16); // width
        bytes.push(16); // height
        bytes.push(0); // color count
        bytes.push(0); // reserved
        bytes.extend_from_slice(&1u16.to_le_bytes()); // planes
        bytes.extend_from_slice(&32u16.to_le_bytes()); // bit count
        bytes.extend_from_slice(&(image_data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&offset.to_le_bytes());

        bytes.extend_from_slice(&image_data);
        bytes
    }

    #[test]
    fn parses_minimal_valid_ico() {
        let bytes = sample_ico_bytes();
        let images = parse_ico(&bytes).expect("well-formed ICO should parse");

        assert_eq!(images.len(), 1);
        assert_eq!(images[0].width, 16);
        assert_eq!(images[0].height, 16);
        assert_eq!(images[0].color_count, 0);
        assert_eq!(images[0].planes, 1);
        assert_eq!(images[0].bit_count, 32);
        assert_eq!(images[0].data, vec![0xAA, 0xBB, 0xCC, 0xDD, 0x11, 0x22, 0x33, 0x44]);
    }

    #[test]
    fn rejects_wrong_header() {
        // Right length, but not the ICO magic (reserved=0, type=1).
        let bytes = vec![1, 2, 3, 4, 5, 6];
        assert!(parse_ico(&bytes).is_err());
    }

    #[test]
    fn rejects_empty_file() {
        assert!(parse_ico(&[]).is_err());
    }

    #[test]
    fn rejects_zero_images() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes()); // count = 0
        assert!(parse_ico(&bytes).is_err());
    }

    #[test]
    fn rejects_truncated_directory_table() {
        // Header claims one entry, but no entry bytes actually follow.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        assert!(parse_ico(&bytes).is_err());
    }

    #[test]
    fn rejects_image_data_past_end_of_file() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.push(16);
        bytes.push(16);
        bytes.push(0);
        bytes.push(0);
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&32u16.to_le_bytes());
        bytes.extend_from_slice(&100u32.to_le_bytes()); // claims far more data than exists
        bytes.extend_from_slice(&22u32.to_le_bytes());
        assert!(parse_ico(&bytes).is_err());
    }

    #[test]
    fn group_icon_data_has_the_shape_windows_expects() {
        let images = parse_ico(&sample_ico_bytes()).unwrap();
        let ids = vec![101u16];
        let group = build_group_icon_data(&images, &ids);

        // GRPICONDIR header (6 bytes) + one GRPICONDIRENTRY (14 bytes).
        assert_eq!(group.len(), 6 + 14);
        assert_eq!(&group[0..2], &0u16.to_le_bytes(), "reserved");
        assert_eq!(&group[2..4], &1u16.to_le_bytes(), "type = icon");
        assert_eq!(&group[4..6], &1u16.to_le_bytes(), "image count");
        assert_eq!(group[6], 16, "width");
        assert_eq!(group[7], 16, "height");
        assert_eq!(
            &group[12..16],
            &(images[0].data.len() as u32).to_le_bytes(),
            "bytesInRes"
        );
        // This is the field that actually differs from a plain ICONDIRENTRY:
        // a resource ID here, not a byte offset into a file.
        assert_eq!(&group[16..18], &101u16.to_le_bytes(), "resource id");
    }
}
