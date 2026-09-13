//! Structural Mach-O parsing for the macOS bottle patcher.
//!
//! Rewriting Homebrew paths into a bottle is a byte-level edit of a binary
//! that is already laid out, so the only thing between a working keg and a
//! silently corrupted one is knowing where writing is safe. This module walks
//! the Mach-O container (thin or universal) and reports only the ranges that
//! hold NUL-terminated C strings: load command string fields and
//! `S_CSTRING_LITERALS` sections. Code, pointer tables, length-prefixed Rust
//! and Go string data and the `__LINKEDIT` signature blob are never reported,
//! so a path that merely happens to appear there is left alone instead of
//! being rewritten into garbage.

use std::fmt;

use object::macho;

/// What kind of storage a [`Region`] describes, which decides whether a
/// replacement string is allowed to grow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RegionKind {
    /// An `lc_str` field: one string padded with NULs to the end of its load
    /// command, so a replacement may use the padding.
    LoadCommand,
    /// A C string literal section: strings are packed back to back, so a
    /// replacement may only shrink into the bytes it already owns.
    CStrings,
}

/// A byte range of the file (absolute, across all architecture slices) that
/// holds NUL-terminated C strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Region {
    pub start: usize,
    pub end: usize,
    pub kind: RegionKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MachoError {
    /// The file is not a Mach-O image at all.
    NotMacho,
    /// The file claims to be Mach-O but its structure does not hold up.
    Malformed(&'static str),
}

impl fmt::Display for MachoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MachoError::NotMacho => write!(f, "not a Mach-O binary"),
            MachoError::Malformed(what) => write!(f, "malformed Mach-O binary: {what}"),
        }
    }
}

/// Result of rewriting the strings in a set of regions.
#[derive(Debug, Default)]
pub(crate) struct Rewrite {
    /// How many strings were rewritten in place.
    pub patched: usize,
    /// Strings that needed rewriting but whose replacement did not fit in the
    /// space available. These are reported rather than truncated.
    pub skipped: Vec<String>,
}

/// The first four bytes read big-endian, which is how Mach-O magic is
/// compared: the byte order of the magic itself tells us the file's byte order.
fn magic(data: &[u8]) -> Option<u32> {
    let bytes = data.get(..4)?;
    Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Whether `data` starts with a thin Mach-O or universal binary magic.
pub(crate) fn is_macho(data: &[u8]) -> bool {
    matches!(
        magic(data),
        Some(
            macho::MH_MAGIC
                | macho::MH_CIGAM
                | macho::MH_MAGIC_64
                | macho::MH_CIGAM_64
                | macho::FAT_MAGIC
                | macho::FAT_MAGIC_64
        )
    )
}

/// Byte order of one Mach-O image.
#[derive(Clone, Copy)]
struct Endian {
    little: bool,
}

impl Endian {
    const BIG: Endian = Endian { little: false };

    fn u32(self, data: &[u8], off: usize) -> Result<u32, MachoError> {
        let b = data
            .get(off..off + 4)
            .ok_or(MachoError::Malformed("truncated structure"))?;
        let raw = [b[0], b[1], b[2], b[3]];
        Ok(if self.little {
            u32::from_le_bytes(raw)
        } else {
            u32::from_be_bytes(raw)
        })
    }

    fn u64(self, data: &[u8], off: usize) -> Result<u64, MachoError> {
        let b = data
            .get(off..off + 8)
            .ok_or(MachoError::Malformed("truncated structure"))?;
        let raw = [b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]];
        Ok(if self.little {
            u64::from_le_bytes(raw)
        } else {
            u64::from_be_bytes(raw)
        })
    }
}

/// Every byte range of `data` that may be rewritten as C string storage.
///
/// Universal binaries contribute the regions of each architecture slice, with
/// offsets translated to the enclosing file.
pub(crate) fn patchable_regions(data: &[u8]) -> Result<Vec<Region>, MachoError> {
    let mut regions = Vec::new();
    match magic(data).ok_or(MachoError::NotMacho)? {
        macho::FAT_MAGIC => fat_regions(data, false, &mut regions)?,
        macho::FAT_MAGIC_64 => fat_regions(data, true, &mut regions)?,
        _ => thin_regions(data, 0, data.len(), &mut regions)?,
    }
    regions.sort_by_key(|r| r.start);
    Ok(regions)
}

/// Walk the architecture slices of a universal binary.
fn fat_regions(data: &[u8], is_64: bool, out: &mut Vec<Region>) -> Result<(), MachoError> {
    let nfat = Endian::BIG.u32(data, 4)?;
    let entry_size = if is_64 { 32 } else { 20 };

    let mut entry = 8usize;
    for _ in 0..nfat {
        let (offset, size) = if is_64 {
            (
                Endian::BIG.u64(data, entry + 8)? as usize,
                Endian::BIG.u64(data, entry + 16)? as usize,
            )
        } else {
            (
                Endian::BIG.u32(data, entry + 8)? as usize,
                Endian::BIG.u32(data, entry + 12)? as usize,
            )
        };

        let end = offset.checked_add(size).ok_or(MachoError::Malformed(
            "architecture slice overflows the file",
        ))?;
        if end > data.len() {
            return Err(MachoError::Malformed(
                "architecture slice extends past the end of the file",
            ));
        }

        // A slice that is not itself Mach-O (some toolchains stash other
        // payloads in a universal wrapper) simply has nothing to patch.
        match thin_regions(data, offset, end, out) {
            Ok(()) | Err(MachoError::NotMacho) => {}
            Err(e) => return Err(e),
        }

        entry += entry_size;
    }

    Ok(())
}

/// Walk one thin Mach-O image occupying `data[start..end]`.
fn thin_regions(
    data: &[u8],
    start: usize,
    end: usize,
    out: &mut Vec<Region>,
) -> Result<(), MachoError> {
    let image = data.get(start..end).ok_or(MachoError::Malformed(
        "image extends past the end of the file",
    ))?;

    let (endian, is_64) = match magic(image).ok_or(MachoError::NotMacho)? {
        macho::MH_MAGIC => (Endian { little: false }, false),
        macho::MH_CIGAM => (Endian { little: true }, false),
        macho::MH_MAGIC_64 => (Endian { little: false }, true),
        macho::MH_CIGAM_64 => (Endian { little: true }, true),
        _ => return Err(MachoError::NotMacho),
    };

    let header_size: usize = if is_64 { 32 } else { 28 };
    let ncmds = endian.u32(image, 16)?;
    let sizeofcmds = endian.u32(image, 20)? as usize;
    let commands_end = header_size
        .checked_add(sizeofcmds)
        .filter(|e| *e <= image.len())
        .ok_or(MachoError::Malformed("load commands extend past the image"))?;

    let mut offset = header_size;
    for _ in 0..ncmds {
        let cmd = endian.u32(image, offset)?;
        let cmdsize = endian.u32(image, offset + 4)? as usize;
        if cmdsize < 8 || offset + cmdsize > commands_end {
            return Err(MachoError::Malformed("load command overruns the header"));
        }

        if cmd == macho::LC_SEGMENT || cmd == macho::LC_SEGMENT_64 {
            segment_regions(
                image,
                start,
                endian,
                offset,
                cmdsize,
                cmd == macho::LC_SEGMENT_64,
                out,
            )?;
        } else if has_lc_str(cmd) {
            // `lc_str` is an offset from the start of its own load command;
            // the string runs to the end of the command, NUL padded.
            let str_offset = endian.u32(image, offset + 8)? as usize;
            if (12..cmdsize).contains(&str_offset) {
                out.push(Region {
                    start: start + offset + str_offset,
                    end: start + offset + cmdsize,
                    kind: RegionKind::LoadCommand,
                });
            }
        }

        offset += cmdsize;
    }

    Ok(())
}

/// Load commands whose payload begins with an `lc_str` at byte 8.
fn has_lc_str(cmd: u32) -> bool {
    matches!(
        cmd,
        macho::LC_ID_DYLIB
            | macho::LC_LOAD_DYLIB
            | macho::LC_LOAD_WEAK_DYLIB
            | macho::LC_REEXPORT_DYLIB
            | macho::LC_LAZY_LOAD_DYLIB
            | macho::LC_LOAD_UPWARD_DYLIB
            | macho::LC_RPATH
            | macho::LC_ID_DYLINKER
            | macho::LC_LOAD_DYLINKER
            | macho::LC_DYLD_ENVIRONMENT
            | macho::LC_SUB_FRAMEWORK
            | macho::LC_SUB_UMBRELLA
            | macho::LC_SUB_CLIENT
            | macho::LC_SUB_LIBRARY
    )
}

/// Collect the C string literal sections of one segment load command.
fn segment_regions(
    image: &[u8],
    image_start: usize,
    endian: Endian,
    command: usize,
    cmdsize: usize,
    is_64: bool,
    out: &mut Vec<Region>,
) -> Result<(), MachoError> {
    let (header_size, section_size, nsects_at) = if is_64 { (72, 80, 64) } else { (56, 68, 48) };
    let nsects = endian.u32(image, command + nsects_at)? as usize;

    for index in 0..nsects {
        let section = command + header_size + index * section_size;
        if section + section_size > command + cmdsize {
            return Err(MachoError::Malformed("section table overruns its segment"));
        }

        let (size, offset, flags) = if is_64 {
            (
                endian.u64(image, section + 40)? as usize,
                endian.u32(image, section + 48)? as usize,
                endian.u32(image, section + 64)?,
            )
        } else {
            (
                endian.u32(image, section + 36)? as usize,
                endian.u32(image, section + 40)? as usize,
                endian.u32(image, section + 56)?,
            )
        };

        if flags & macho::SECTION_TYPE != macho::S_CSTRING_LITERALS || size == 0 {
            continue;
        }

        let end = offset
            .checked_add(size)
            .filter(|e| *e <= image.len())
            .ok_or(MachoError::Malformed("section extends past the image"))?;

        out.push(Region {
            start: image_start + offset,
            end: image_start + end,
            kind: RegionKind::CStrings,
        });
    }

    Ok(())
}

/// Rewrite the NUL-terminated strings inside `regions` in place.
///
/// `rewrite` is handed each whole string and returns its replacement, or
/// `None` to leave it alone. Replacing the whole string (rather than splicing
/// bytes into the middle of it) is what keeps a shrinking path from leaving an
/// embedded NUL behind, which would truncate the string at load time.
pub(crate) fn rewrite_strings<F>(data: &mut [u8], regions: &[Region], rewrite: F) -> Rewrite
where
    F: Fn(&str) -> Option<String>,
{
    let mut report = Rewrite::default();

    for region in regions {
        let end = region.end.min(data.len());
        let mut cursor = region.start;

        while cursor < end {
            if data[cursor] == 0 {
                cursor += 1;
                continue;
            }

            let start = cursor;
            let mut terminator = cursor;
            while terminator < end && data[terminator] != 0 {
                terminator += 1;
            }
            if terminator == end {
                // Not NUL terminated inside its own region, so we cannot treat
                // it as a C string.
                break;
            }

            let capacity = match region.kind {
                RegionKind::CStrings => terminator - start,
                RegionKind::LoadCommand => {
                    // The padding after the string belongs to this command, so
                    // a longer path may use it as long as a terminator remains.
                    let mut padding = terminator;
                    while padding < end && data[padding] == 0 {
                        padding += 1;
                    }
                    padding - start - 1
                }
            };

            if let Ok(text) = std::str::from_utf8(&data[start..terminator])
                && let Some(replacement) = rewrite(text)
            {
                if replacement.len() <= capacity {
                    data[start..start + replacement.len()].copy_from_slice(replacement.as_bytes());
                    let tail = start + replacement.len();
                    if tail < terminator {
                        data[tail..terminator].fill(0);
                    }
                    report.patched += 1;
                } else {
                    report.skipped.push(text.to_string());
                }
            }

            cursor = terminator + 1;
        }
    }

    report
}

/// Whether `position` falls inside one of `regions`, which must be sorted by
/// `start` (as [`patchable_regions`] returns them).
pub(crate) fn contains(regions: &[Region], position: usize) -> bool {
    let after = regions.partition_point(|r| r.start <= position);
    regions[..after].iter().any(|r| position < r.end)
}

#[cfg(test)]
pub(crate) mod test_support {
    use object::macho;

    /// A synthetic 64-bit little-endian Mach-O image, enough of one for the
    /// patcher to walk: a `__TEXT` segment with a `__text` section (never
    /// patchable) and a `__cstring` section (patchable), plus optional rpaths.
    pub(crate) struct TestMacho {
        pub text: Vec<u8>,
        pub cstrings: Vec<u8>,
        pub rpaths: Vec<String>,
    }

    impl TestMacho {
        pub(crate) fn new(text: &[u8], cstrings: &[u8]) -> Self {
            Self {
                text: text.to_vec(),
                cstrings: cstrings.to_vec(),
                rpaths: Vec::new(),
            }
        }

        pub(crate) fn with_rpath(mut self, rpath: &str) -> Self {
            self.rpaths.push(rpath.to_string());
            self
        }

        pub(crate) fn build(&self) -> Vec<u8> {
            const HEADER: usize = 32;
            const SEGMENT: usize = 72;
            const SECTION: usize = 80;

            let rpath_commands: Vec<Vec<u8>> =
                self.rpaths.iter().map(|p| rpath_command(p)).collect();
            let segment_size = SEGMENT + 2 * SECTION;
            let sizeofcmds = segment_size + rpath_commands.iter().map(|c| c.len()).sum::<usize>();
            let data_start = HEADER + sizeofcmds;
            let text_offset = data_start;
            let cstring_offset = text_offset + self.text.len();

            let mut out = Vec::new();
            // mach_header_64
            out.extend(macho::MH_MAGIC_64.to_le_bytes());
            out.extend(0x0100_000cu32.to_le_bytes()); // CPU_TYPE_ARM64
            out.extend(0u32.to_le_bytes()); // cpusubtype
            out.extend(2u32.to_le_bytes()); // MH_EXECUTE
            out.extend(((1 + rpath_commands.len()) as u32).to_le_bytes());
            out.extend((sizeofcmds as u32).to_le_bytes());
            out.extend(0u32.to_le_bytes()); // flags
            out.extend(0u32.to_le_bytes()); // reserved

            // LC_SEGMENT_64 __TEXT
            out.extend(macho::LC_SEGMENT_64.to_le_bytes());
            out.extend((segment_size as u32).to_le_bytes());
            out.extend(fixed16("__TEXT"));
            out.extend(0u64.to_le_bytes()); // vmaddr
            out.extend(0u64.to_le_bytes()); // vmsize
            out.extend(0u64.to_le_bytes()); // fileoff
            out.extend(((cstring_offset + self.cstrings.len()) as u64).to_le_bytes());
            out.extend(7u32.to_le_bytes()); // maxprot
            out.extend(5u32.to_le_bytes()); // initprot
            out.extend(2u32.to_le_bytes()); // nsects
            out.extend(0u32.to_le_bytes()); // flags
            out.extend(section64(
                "__text",
                "__TEXT",
                self.text.len(),
                text_offset,
                0x8000_0400, // S_REGULAR | S_ATTR_PURE_INSTRUCTIONS
            ));
            out.extend(section64(
                "__cstring",
                "__TEXT",
                self.cstrings.len(),
                cstring_offset,
                macho::S_CSTRING_LITERALS,
            ));

            for command in &rpath_commands {
                out.extend(command);
            }

            assert_eq!(out.len(), data_start, "load commands sized incorrectly");
            out.extend(&self.text);
            out.extend(&self.cstrings);
            out
        }
    }

    fn rpath_command(path: &str) -> Vec<u8> {
        let cmdsize = (12 + path.len() + 1).next_multiple_of(8);
        let mut out = Vec::new();
        out.extend(macho::LC_RPATH.to_le_bytes());
        out.extend((cmdsize as u32).to_le_bytes());
        out.extend(12u32.to_le_bytes()); // lc_str offset
        out.extend(path.as_bytes());
        out.resize(cmdsize, 0);
        out
    }

    fn section64(name: &str, segment: &str, size: usize, offset: usize, flags: u32) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend(fixed16(name));
        out.extend(fixed16(segment));
        out.extend(0u64.to_le_bytes()); // addr
        out.extend((size as u64).to_le_bytes());
        out.extend((offset as u32).to_le_bytes());
        out.extend(0u32.to_le_bytes()); // align
        out.extend(0u32.to_le_bytes()); // reloff
        out.extend(0u32.to_le_bytes()); // nreloc
        out.extend(flags.to_le_bytes());
        out.extend([0u8; 12]); // reserved1..3
        out
    }

    fn fixed16(name: &str) -> [u8; 16] {
        let mut out = [0u8; 16];
        let bytes = name.as_bytes();
        out[..bytes.len()].copy_from_slice(bytes);
        out
    }

    /// Wrap thin images into a universal binary, page aligning each slice.
    pub(crate) fn fat(images: &[Vec<u8>]) -> Vec<u8> {
        let mut header = Vec::new();
        header.extend(macho::FAT_MAGIC.to_be_bytes());
        header.extend((images.len() as u32).to_be_bytes());

        let mut offset = (8 + images.len() * 20).next_multiple_of(0x4000);
        let mut body: Vec<u8> = Vec::new();
        for (index, image) in images.iter().enumerate() {
            header.extend(0x0100_000cu32.to_be_bytes()); // cputype
            header.extend((index as u32).to_be_bytes()); // cpusubtype
            header.extend((offset as u32).to_be_bytes());
            header.extend((image.len() as u32).to_be_bytes());
            header.extend(14u32.to_be_bytes()); // align

            body.resize(offset - (8 + images.len() * 20), 0);
            body.extend(image);
            offset = (offset + image.len()).next_multiple_of(0x4000);
        }

        header.extend(body);
        header
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{TestMacho, fat};
    use super::*;

    const CSTRINGS: &[u8] = b"/opt/homebrew/opt/git/libexec/git-core\0plain\0";
    const TEXT: &[u8] = b"\x00\x01/opt/homebrew/not/a/string\x02\x03";

    fn sample() -> Vec<u8> {
        TestMacho::new(TEXT, CSTRINGS)
            .with_rpath("/opt/homebrew/lib")
            .build()
    }

    #[test]
    fn reports_cstring_sections_and_load_command_strings() {
        let image = sample();
        let regions = patchable_regions(&image).expect("image should parse");

        assert_eq!(regions.len(), 2);
        assert!(regions.iter().any(|r| r.kind == RegionKind::LoadCommand
            && image[r.start..].starts_with(b"/opt/homebrew/lib\0")));
        assert!(
            regions
                .iter()
                .any(|r| r.kind == RegionKind::CStrings && image[r.start..r.end] == *CSTRINGS)
        );
    }

    #[test]
    fn text_section_bytes_are_never_patchable() {
        let image = sample();
        let regions = patchable_regions(&image).expect("image should parse");

        let text_at = image
            .windows(TEXT.len())
            .position(|w| w == TEXT)
            .expect("__text contents should be present");
        for offset in 0..TEXT.len() {
            assert!(
                !contains(&regions, text_at + offset),
                "__text byte {offset} must not be patchable"
            );
        }
    }

    #[test]
    fn rejects_non_macho_and_truncated_images() {
        assert_eq!(
            patchable_regions(b"not a binary"),
            Err(MachoError::NotMacho)
        );
        assert!(!is_macho(b"not a binary"));

        let mut truncated = sample();
        truncated.truncate(40);
        assert!(matches!(
            patchable_regions(&truncated),
            Err(MachoError::Malformed(_))
        ));
    }

    #[test]
    fn walks_every_slice_of_a_universal_binary() {
        let image = fat(&[sample(), sample()]);
        assert!(is_macho(&image));

        let regions = patchable_regions(&image).expect("universal binary should parse");
        assert_eq!(regions.len(), 4);
        assert!(regions.windows(2).all(|w| w[0].start <= w[1].start));
    }

    #[test]
    fn shrinking_a_string_moves_its_tail_instead_of_splicing_nuls() {
        let mut image = sample();
        let regions = patchable_regions(&image).expect("image should parse");

        let report = rewrite_strings(&mut image, &regions, |s| {
            s.strip_prefix("/opt/homebrew")
                .map(|rest| format!("/zb{rest}"))
        });

        assert_eq!(report.patched, 2);
        assert!(report.skipped.is_empty());
        assert!(
            find(&image, b"/zb/opt/git/libexec/git-core\0").is_some(),
            "the whole path must survive, not just its first component"
        );
        assert!(find(&image, b"/zb/lib\0").is_some());

        // The bytes freed by the shorter path are cleared, and the untouched
        // neighbour string stays exactly where it was.
        let mut expected = b"/zb/opt/git/libexec/git-core".to_vec();
        expected.resize(CSTRINGS.len() - b"plain\0".len(), 0);
        expected.extend(b"plain\0");
        assert!(find(&image, &expected).is_some());
    }

    #[test]
    fn a_longer_string_is_reported_rather_than_truncated() {
        let mut image = sample();
        let regions = patchable_regions(&image).expect("image should parse");
        let original = image.clone();

        let report = rewrite_strings(&mut image, &regions, |s| {
            s.strip_prefix("/opt/homebrew")
                .map(|rest| format!("/a/much/longer/zerobrew/prefix{rest}"))
        });

        assert_eq!(report.patched, 0);
        assert_eq!(
            report.skipped,
            vec![
                "/opt/homebrew/lib".to_string(),
                "/opt/homebrew/opt/git/libexec/git-core".to_string(),
            ]
        );
        assert_eq!(image, original, "nothing should have been written");
    }

    #[test]
    fn a_load_command_string_may_grow_into_its_padding() {
        // "/opt/homebrew/lib" occupies 17 bytes plus a NUL in a command padded
        // to 32 bytes, so 19 bytes of string still fit.
        let mut image = TestMacho::new(b"", b"")
            .with_rpath("/opt/homebrew/lib")
            .build();
        let regions = patchable_regions(&image).expect("image should parse");

        let report = rewrite_strings(&mut image, &regions, |s| {
            s.strip_prefix("/opt/homebrew")
                .map(|rest| format!("/opt/zerobrew{rest}"))
        });

        assert_eq!(report.patched, 1);
        assert!(find(&image, b"/opt/zerobrew/lib\0").is_some());
    }

    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }
}
