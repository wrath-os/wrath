//! Unified kernel images: systemd's EFI stub with the kernel, the initrd, the command line and
//! the system's os-release added as sections, one file the firmware or systemd-boot starts and
//! that can later be signed as a whole.

/// The parts of a unified kernel image.
pub struct Parts<'a> {
    pub os_release: &'a [u8],
    pub cmdline: &'a [u8],
    pub initrd: &'a [u8],
    pub linux: &'a [u8],
    /// The public key the TPM policy is signed with, in PEM, which the stub measures too.
    pub pcr_public_key: Option<&'a [u8]>,
    /// The signed TPM policy for the PCR 11 values the image gives, as `systemd-measure sign`
    /// writes it, which the stub doesn't measure.
    pub pcr_signature: Option<&'a [u8]>,
}

/// The first systemd-boot that reads an entry's `uki` line, and counts tries for it.
const UKI_LINES_SINCE: u32 = 258;

/// The line of a boot entry that starts the image at `path` on the ESP, for the systemd-boot in
/// `loader`: `uki`, or `efi` for one too old to know `uki`, which starts it all the same but
/// counts no tries. A loader whose version can't be read gets `efi` too.
pub fn entry_line(loader: &[u8], path: &str) -> String {
    let marker = b"#### LoaderInfo: systemd-boot ";
    let major = loader
        .windows(marker.len())
        .position(|window| window == marker)
        .map(|at| &loader[at + marker.len()..])
        .and_then(|rest| {
            let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
            std::str::from_utf8(&rest[..digits])
                .ok()?
                .parse::<u32>()
                .ok()
        });
    match major {
        Some(major) if major >= UKI_LINES_SINCE => format!("uki {path}\n"),
        _ => format!("efi {path}\n"),
    }
}

const SECTION_HEADER: usize = 40;
/// Initialized data, readable.
const DATA: u32 = 0x4000_0040;
/// PE32+, the format of 64-bit EFI programs.
const PE32_PLUS: u16 = 0x20b;
/// The certificate table's entry among the optional header's data directories.
const CERTIFICATES: usize = 112 + 4 * 8;

fn u16_at(bytes: &[u8], at: usize) -> Result<u16, String> {
    bytes
        .get(at..at + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .ok_or_else(|| "the stub is cut short".to_owned())
}

fn u32_at(bytes: &[u8], at: usize) -> Result<u32, String> {
    bytes
        .get(at..at + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or_else(|| "the stub is cut short".to_owned())
}

fn put_u16(bytes: &mut [u8], at: usize, value: u16) {
    bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn align(value: usize, to: usize) -> usize {
    value.next_multiple_of(to)
}

/// `stub`, systemd's `linuxx64.efi.stub`, with `parts` added as the sections it looks for:
/// `.osrel`, `.cmdline`, `.initrd` and `.linux`, the kernel last, as ukify places it.
pub fn assemble(stub: &[u8], parts: &Parts) -> Result<Vec<u8>, String> {
    if stub.get(..2) != Some(b"MZ") {
        return Err("the stub isn't an EFI program".to_owned());
    }
    let pe = u32_at(stub, 0x3c)? as usize;
    if stub.get(pe..pe + 4) != Some(b"PE\0\0") {
        return Err("the stub isn't an EFI program".to_owned());
    }
    let coff = pe + 4;
    let count = u16_at(stub, coff + 2)? as usize;
    let optional = coff + 20;
    if u16_at(stub, optional)? != PE32_PLUS {
        return Err("the stub isn't a 64-bit EFI program".to_owned());
    }
    let section_alignment = u32_at(stub, optional + 32)? as usize;
    let file_alignment = u32_at(stub, optional + 36)? as usize;
    let headers_size = u32_at(stub, optional + 60)? as usize;
    if u32_at(stub, optional + CERTIFICATES + 4)? != 0 {
        return Err("the stub is signed, and adding to it would break the signature".to_owned());
    }
    let table = optional + u16_at(stub, coff + 16)? as usize;

    let mut image_end = 0;
    let mut file_end = headers_size;
    let mut first_data = usize::MAX;
    for n in 0..count {
        let header = table + n * SECTION_HEADER;
        let (virtual_size, address) = (u32_at(stub, header + 8)?, u32_at(stub, header + 12)?);
        let (raw_size, raw_at) = (u32_at(stub, header + 16)?, u32_at(stub, header + 20)?);
        image_end = image_end.max(address as usize + virtual_size as usize);
        file_end = file_end.max(raw_at as usize + raw_size as usize);
        if raw_size > 0 {
            first_data = first_data.min(raw_at as usize);
        }
    }
    let mut sections: Vec<(&[u8; 8], &[u8])> = vec![
        (b".osrel\0\0", parts.os_release),
        (b".cmdline", parts.cmdline),
        (b".initrd\0", parts.initrd),
    ];
    if let Some(key) = parts.pcr_public_key {
        sections.push((b".pcrpkey", key));
    }
    if let Some(signature) = parts.pcr_signature {
        sections.push((b".pcrsig\0", signature));
    }
    sections.push((b".linux\0\0", parts.linux));
    let new_table = table + count * SECTION_HEADER;
    if new_table + sections.len() * SECTION_HEADER > headers_size.min(first_data) {
        return Err("the stub has no room for more sections".to_owned());
    }

    let mut out = stub[..file_end.min(stub.len())].to_vec();
    out.resize(align(file_end, file_alignment), 0);
    let mut address = align(image_end, section_alignment);
    for (n, (name, data)) in sections.iter().enumerate() {
        let raw_at = out.len();
        out.extend_from_slice(data);
        out.resize(align(out.len(), file_alignment), 0);
        let header = new_table + n * SECTION_HEADER;
        let fields = [
            data.len(),
            address,
            align(data.len(), file_alignment),
            raw_at,
        ];
        out[header..header + 8].copy_from_slice(*name);
        for (i, field) in fields.into_iter().enumerate() {
            let field = u32::try_from(field).map_err(|_| "the image is over 4 GB".to_owned())?;
            put_u32(&mut out, header + 8 + i * 4, field);
        }
        out[header + 24..header + 36].fill(0);
        put_u32(&mut out, header + 36, DATA);
        address = align(address + data.len(), section_alignment);
    }
    let image_size = u32::try_from(address).map_err(|_| "the image is over 4 GB".to_owned())?;
    put_u16(&mut out, coff + 2, (count + sections.len()) as u16);
    put_u32(&mut out, optional + 56, image_size);
    // The firmware doesn't check it, and signing works it out again.
    put_u32(&mut out, optional + 64, 0);
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A PE32+ program like systemd's stub: headers with room to spare and one `.text` section.
    pub(crate) fn stub() -> Vec<u8> {
        let mut bytes = vec![0; 0x600];
        bytes[..2].copy_from_slice(b"MZ");
        put_u32(&mut bytes, 0x3c, 0x80);
        bytes[0x80..0x84].copy_from_slice(b"PE\0\0");
        let (coff, optional) = (0x84, 0x98);
        put_u16(&mut bytes, coff + 2, 1);
        put_u16(&mut bytes, coff + 16, 240);
        put_u16(&mut bytes, optional, PE32_PLUS);
        put_u32(&mut bytes, optional + 32, 0x1000);
        put_u32(&mut bytes, optional + 36, 0x200);
        put_u32(&mut bytes, optional + 56, 0x2000);
        put_u32(&mut bytes, optional + 60, 0x400);
        let text = optional + 240;
        bytes[text..text + 8].copy_from_slice(b".text\0\0\0");
        put_u32(&mut bytes, text + 8, 0x10);
        put_u32(&mut bytes, text + 12, 0x1000);
        put_u32(&mut bytes, text + 16, 0x200);
        put_u32(&mut bytes, text + 20, 0x400);
        bytes[0x400..0x410].fill(0xcc);
        bytes
    }

    fn parts() -> Parts<'static> {
        Parts {
            os_release: b"NAME=wrath\n",
            cmdline: b"init=/wrath/store/abc-system/init",
            initrd: &[7; 0x300],
            linux: &[9; 0x1234],
            pcr_public_key: None,
            pcr_signature: None,
        }
    }

    /// The sections of a PE program, by name, with their address, size and contents.
    fn sections(image: &[u8]) -> Vec<(String, usize, Vec<u8>)> {
        let coff = u32_at(image, 0x3c).unwrap() as usize + 4;
        let table = coff + 20 + u16_at(image, coff + 16).unwrap() as usize;
        (0..u16_at(image, coff + 2).unwrap() as usize)
            .map(|n| {
                let at = table + n * SECTION_HEADER;
                let name = String::from_utf8_lossy(&image[at..at + 8]);
                let size = u32_at(image, at + 8).unwrap() as usize;
                let raw = u32_at(image, at + 20).unwrap() as usize;
                (
                    name.trim_end_matches('\0').to_owned(),
                    u32_at(image, at + 12).unwrap() as usize,
                    image[raw..raw + size].to_vec(),
                )
            })
            .collect()
    }

    #[test]
    fn the_parts_follow_the_stubs_own_sections_with_the_kernel_last() {
        let image = assemble(&stub(), &parts()).unwrap();
        let found = sections(&image);
        let names: Vec<&str> = found.iter().map(|(name, ..)| name.as_str()).collect();
        assert_eq!(names, [".text", ".osrel", ".cmdline", ".initrd", ".linux"]);
        assert_eq!(found[0].2, [0xcc; 0x10]);
        assert_eq!(found[2].2, b"init=/wrath/store/abc-system/init");
        assert_eq!(found[4].2, [9; 0x1234]);
    }

    #[test]
    fn a_signed_policy_and_its_key_go_ahead_of_the_kernel() {
        let parts = Parts {
            pcr_public_key: Some(b"key"),
            pcr_signature: Some(b"{}"),
            ..parts()
        };
        let image = assemble(&stub(), &parts).unwrap();
        let found = sections(&image);
        let names: Vec<&str> = found.iter().map(|(name, ..)| name.as_str()).collect();
        assert_eq!(
            names,
            [
                ".text", ".osrel", ".cmdline", ".initrd", ".pcrpkey", ".pcrsig", ".linux"
            ]
        );
        assert_eq!(found[4].2, b"key");
    }

    #[test]
    fn each_section_starts_on_its_own_page_after_the_last() {
        let image = assemble(&stub(), &parts()).unwrap();
        let addresses: Vec<usize> = sections(&image).iter().map(|(_, at, _)| *at).collect();
        assert_eq!(addresses, [0x1000, 0x2000, 0x3000, 0x4000, 0x5000]);
        let optional = u32_at(&image, 0x3c).unwrap() as usize + 24;
        assert_eq!(u32_at(&image, optional + 56).unwrap(), 0x7000);
        assert_eq!(image.len() % 0x200, 0);
    }

    #[test]
    fn a_boot_loader_too_old_for_uki_lines_gets_efi_lines() {
        let loader = |version: &str| format!("\0#### LoaderInfo: systemd-boot {version} ####\0");
        let at = "/EFI/wrath/a.efi";
        assert_eq!(
            entry_line(loader("262.2").as_bytes(), at),
            "uki /EFI/wrath/a.efi\n"
        );
        assert_eq!(
            entry_line(loader("258").as_bytes(), at),
            "uki /EFI/wrath/a.efi\n"
        );
        assert_eq!(
            entry_line(loader("257.8").as_bytes(), at),
            "efi /EFI/wrath/a.efi\n"
        );
        assert_eq!(entry_line(b"another loader", at), "efi /EFI/wrath/a.efi\n");
    }

    #[test]
    fn a_signed_stub_or_one_without_room_is_refused() {
        let mut signed = stub();
        put_u32(&mut signed, 0x98 + CERTIFICATES + 4, 0x100);
        assert!(assemble(&signed, &parts()).unwrap_err().contains("signed"));
        let mut full = stub();
        put_u32(
            &mut full,
            0x98 + 60,
            (0x98 + 240 + 2 * SECTION_HEADER) as u32,
        );
        assert!(assemble(&full, &parts()).unwrap_err().contains("no room"));
        assert!(assemble(b"ELF", &parts()).is_err());
    }
}
