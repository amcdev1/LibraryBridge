//! A tiny PNG writer, used only by the `--screenshot` development flag.
//!
//! It emits uncompressed deflate blocks, which is valid but larger than a
//! normal PNG. That is a fair trade to avoid a compression dependency for a
//! feature that only exists to capture the window during development.

fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (index, entry) in table.iter_mut().enumerate() {
        let mut value = index as u32;
        for _ in 0..8 {
            value = if value & 1 != 0 {
                0xedb8_8320 ^ (value >> 1)
            } else {
                value >> 1
            };
        }
        *entry = value;
    }
    let mut crc = 0xffff_ffffu32;
    for byte in data {
        crc = table[((crc ^ *byte as u32) & 0xff) as usize] ^ (crc >> 8);
    }
    crc ^ 0xffff_ffff
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for byte in data {
        a = (a + *byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    let mut with_kind = kind.to_vec();
    with_kind.extend_from_slice(body);
    out.extend_from_slice(&with_kind);
    out.extend_from_slice(&crc32(&with_kind).to_be_bytes());
}

/// `pixels` is RGBA, row major, `width * height * 4` bytes.
pub fn encode(width: usize, height: usize, pixels: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity(height * (1 + width * 4));
    for row in 0..height {
        raw.push(0); // filter: none
        let start = row * width * 4;
        raw.extend_from_slice(&pixels[start..start + width * 4]);
    }

    let mut zlib = vec![0x78, 0x01];
    let mut remaining = raw.as_slice();
    while !remaining.is_empty() {
        let take = remaining.len().min(65535);
        let last = if take == remaining.len() { 1u8 } else { 0u8 };
        zlib.push(last);
        zlib.extend_from_slice(&(take as u16).to_le_bytes());
        zlib.extend_from_slice(&(!(take as u16)).to_le_bytes());
        zlib.extend_from_slice(&remaining[..take]);
        remaining = &remaining[take..];
    }
    zlib.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let mut header = Vec::new();
    header.extend_from_slice(&(width as u32).to_be_bytes());
    header.extend_from_slice(&(height as u32).to_be_bytes());
    header.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA
    chunk(&mut out, b"IHDR", &header);
    chunk(&mut out, b"IDAT", &zlib);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// Average each 2x2 block into one pixel. Screens are captured at the
/// display's own resolution, which is more than a screenshot needs.
pub fn halve(width: usize, height: usize, pixels: &[u8]) -> (usize, usize, Vec<u8>) {
    if width < 2 || height < 2 {
        return (width, height, pixels.to_vec());
    }
    let (out_width, out_height) = (width / 2, height / 2);
    let mut out = Vec::with_capacity(out_width * out_height * 4);
    for row in 0..out_height {
        for column in 0..out_width {
            for channel in 0..4 {
                let at = |r: usize, c: usize| pixels[(r * width + c) * 4 + channel] as u32;
                let total = at(row * 2, column * 2)
                    + at(row * 2, column * 2 + 1)
                    + at(row * 2 + 1, column * 2)
                    + at(row * 2 + 1, column * 2 + 1);
                out.push((total / 4) as u8);
            }
        }
    }
    (out_width, out_height, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_a_structurally_valid_png() {
        let pixels = vec![255u8; 2 * 2 * 4];
        let encoded = encode(2, 2, &pixels);
        assert_eq!(
            &encoded[..8],
            &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]
        );
        assert_eq!(&encoded[12..16], b"IHDR");
        assert!(encoded.windows(4).any(|w| w == b"IDAT"));
        assert_eq!(&encoded[encoded.len() - 8..encoded.len() - 4], b"IEND");
    }

    #[test]
    fn chunk_lengths_and_checksums_line_up() {
        let encoded = encode(3, 2, &vec![7u8; 3 * 2 * 4]);
        let mut at = 8;
        let mut kinds = Vec::new();
        while at + 8 <= encoded.len() {
            let length = u32::from_be_bytes(encoded[at..at + 4].try_into().unwrap()) as usize;
            let kind = &encoded[at + 4..at + 8];
            kinds.push(String::from_utf8_lossy(kind).to_string());
            let body = &encoded[at + 4..at + 8 + length];
            let stored = u32::from_be_bytes(
                encoded[at + 8 + length..at + 12 + length]
                    .try_into()
                    .unwrap(),
            );
            assert_eq!(crc32(body), stored, "bad checksum on chunk");
            at += 12 + length;
        }
        assert_eq!(kinds, ["IHDR", "IDAT", "IEND"]);
        assert_eq!(at, encoded.len());
    }

    #[test]
    fn zlib_stream_carries_the_right_adler_checksum() {
        let raw_pixels = vec![1u8; 4 * 4];
        let encoded = encode(4, 1, &raw_pixels);
        let start = encoded.windows(4).position(|w| w == b"IDAT").unwrap() + 4;
        let length = u32::from_be_bytes(encoded[start - 8..start - 4].try_into().unwrap()) as usize;
        let zlib = &encoded[start..start + length];
        assert_eq!(&zlib[..2], &[0x78, 0x01]);
        let mut expected = vec![0u8];
        expected.extend_from_slice(&raw_pixels);
        assert_eq!(&zlib[zlib.len() - 4..], &adler32(&expected).to_be_bytes());
    }

    #[test]
    fn halving_averages_each_block() {
        // One 2x2 block: values 0, 100, 200 and 255 in every channel.
        let mut pixels = Vec::new();
        for value in [0u8, 100, 200, 255] {
            pixels.extend_from_slice(&[value; 4]);
        }
        let (width, height, out) = halve(2, 2, &pixels);
        assert_eq!((width, height), (1, 1));
        assert_eq!(out, vec![138, 138, 138, 138]);
    }

    #[test]
    fn halving_leaves_tiny_images_alone() {
        let pixels = vec![9u8; 4];
        assert_eq!(halve(1, 1, &pixels), (1, 1, pixels));
    }
}
