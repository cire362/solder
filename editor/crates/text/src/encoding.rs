//! What a file's bytes are as text, and the same text as bytes again.
//!
//! A file is read in the encoding it is written in and saved in that same
//! one, so that opening and saving it changes nothing it did not have to.
//! The ones known are UTF-8 with or without its mark, UTF-16 with its
//! mark, and the two single-byte ones files of this part of the world are
//! still found in. The single-byte ones give a character for every byte,
//! so whatever was read is written back the same.

/// Where a file that is not text is looked at for the byte text never has.
const SNIFF: usize = 8 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Encoding {
    #[default]
    Utf8,
    /// UTF-8 that begins with the byte order mark, which is kept.
    Utf8Bom,
    Utf16Le,
    Utf16Be,
    /// Cyrillic, one byte a letter.
    Windows1251,
    /// Western European, one byte a letter.
    Windows1252,
}

impl Encoding {
    pub const ALL: [Encoding; 6] = [
        Encoding::Utf8,
        Encoding::Utf8Bom,
        Encoding::Utf16Le,
        Encoding::Utf16Be,
        Encoding::Windows1251,
        Encoding::Windows1252,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Utf8 => "UTF-8",
            Self::Utf8Bom => "UTF-8 with BOM",
            Self::Utf16Le => "UTF-16 LE",
            Self::Utf16Be => "UTF-16 BE",
            Self::Windows1251 => "Windows-1251",
            Self::Windows1252 => "Windows-1252",
        }
    }
}

/// A file as it was read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decoded {
    pub text: String,
    pub encoding: Encoding,
    /// It has bytes no text has: it is shown, and is not to be edited.
    pub binary: bool,
}

/// What 0x80 to 0x9F are in Windows-1252. The five it leaves undefined
/// are the control characters of the same number, so every byte reads.
const WESTERN: [u16; 32] = [
    0x20AC, 0x0081, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039,
    0x0152, 0x008D, 0x017D, 0x008F, 0x0090, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014,
    0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x009D, 0x017E, 0x0178,
];

/// What 0x80 to 0xBF are in Windows-1251; 0xC0 to 0xFF are the alphabet
/// in order. The one it leaves undefined (0x98) is read as itself.
const CYRILLIC: [u16; 64] = [
    0x0402, 0x0403, 0x201A, 0x0453, 0x201E, 0x2026, 0x2020, 0x2021, 0x20AC, 0x2030, 0x0409, 0x2039,
    0x040A, 0x040C, 0x040B, 0x040F, 0x0452, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014,
    0x0098, 0x2122, 0x0459, 0x203A, 0x045A, 0x045C, 0x045B, 0x045F, 0x00A0, 0x040E, 0x045E, 0x0408,
    0x00A4, 0x0490, 0x00A6, 0x00A7, 0x0401, 0x00A9, 0x0404, 0x00AB, 0x00AC, 0x00AD, 0x00AE, 0x0407,
    0x00B0, 0x00B1, 0x0406, 0x0456, 0x0491, 0x00B5, 0x00B6, 0x00B7, 0x0451, 0x2116, 0x0454, 0x00BB,
    0x0458, 0x0405, 0x0455, 0x0457,
];

fn one_byte(byte: u8, encoding: Encoding) -> char {
    let code = match (encoding, byte) {
        (_, 0..=0x7F) => byte as u32,
        (Encoding::Windows1251, 0x80..=0xBF) => CYRILLIC[byte as usize - 0x80] as u32,
        (Encoding::Windows1251, _) => 0x0410 + (byte as u32 - 0xC0),
        (_, 0x80..=0x9F) => WESTERN[byte as usize - 0x80] as u32,
        (_, _) => byte as u32,
    };
    char::from_u32(code).unwrap_or(char::REPLACEMENT_CHARACTER)
}

fn to_one_byte(c: char, encoding: Encoding) -> Option<u8> {
    let code = c as u32;
    if code < 0x80 {
        return Some(code as u8);
    }
    let found = |table: &[u16]| table.iter().position(|known| *known as u32 == code);
    match encoding {
        Encoding::Windows1251 => match code {
            0x0410..=0x044F => Some((code - 0x0410) as u8 + 0xC0),
            _ => found(&CYRILLIC).map(|at| at as u8 + 0x80),
        },
        _ => match code {
            0xA0..=0xFF => Some(code as u8),
            _ => found(&WESTERN).map(|at| at as u8 + 0x80),
        },
    }
}

/// Reads bytes as text, finding out what they are written in: by the
/// mark they begin with, then by whether they are UTF-8, then by what
/// their letters look like.
pub fn decode(bytes: &[u8]) -> Decoded {
    let marked = match bytes {
        [0xEF, 0xBB, 0xBF, ..] => Some(Encoding::Utf8Bom),
        [0xFF, 0xFE, ..] => Some(Encoding::Utf16Le),
        [0xFE, 0xFF, ..] => Some(Encoding::Utf16Be),
        _ => None,
    };
    let binary = !matches!(marked, Some(Encoding::Utf16Le | Encoding::Utf16Be))
        && bytes[..bytes.len().min(SNIFF)].contains(&0);
    let encoding = match marked {
        Some(marked) => marked,
        None if std::str::from_utf8(bytes).is_ok() => Encoding::Utf8,
        None if binary => Encoding::Windows1252,
        None => guess(bytes),
    };
    Decoded {
        text: decode_as(bytes, encoding),
        encoding,
        binary,
    }
}

/// Which single-byte encoding bytes that are not UTF-8 are in. In a text
/// in Cyrillic the letters past ASCII stand next to one another, in words;
/// in a Western one they are single letters among ASCII ones.
fn guess(bytes: &[u8]) -> Encoding {
    let letter = |byte: u8| byte >= 0xC0 || byte == 0xA8 || byte == 0xB8;
    let high = bytes.iter().filter(|byte| **byte >= 0x80).count();
    let paired = bytes
        .windows(2)
        .filter(|pair| letter(pair[0]) && letter(pair[1]))
        .count();
    match paired * 2 >= high {
        true => Encoding::Windows1251,
        false => Encoding::Windows1252,
    }
}

/// Reads bytes as text in an encoding that is known. What is not valid in
/// it (UTF-8 and UTF-16 only) reads as the replacement character.
pub fn decode_as(bytes: &[u8], encoding: Encoding) -> String {
    match encoding {
        Encoding::Utf8 => String::from_utf8_lossy(bytes).into_owned(),
        Encoding::Utf8Bom => {
            let text = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
            String::from_utf8_lossy(text).into_owned()
        }
        Encoding::Utf16Le | Encoding::Utf16Be => {
            let little = encoding == Encoding::Utf16Le;
            let mark: &[u8] = if little { &[0xFF, 0xFE] } else { &[0xFE, 0xFF] };
            let text = bytes.strip_prefix(mark).unwrap_or(bytes);
            let units = text.chunks(2).map(|pair| match (pair, little) {
                ([low, high], true) => u16::from_le_bytes([*low, *high]),
                ([high, low], false) => u16::from_be_bytes([*high, *low]),
                // Half a unit at the end is no character.
                _ => 0xFFFD,
            });
            char::decode_utf16(units)
                .map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER))
                .collect()
        }
        Encoding::Windows1251 | Encoding::Windows1252 => {
            bytes.iter().map(|byte| one_byte(*byte, encoding)).collect()
        }
    }
}

/// The bytes of a text in an encoding, with the mark of the ones that
/// have one. `Err` is the first character the encoding has no byte for.
pub fn encode(text: &str, encoding: Encoding) -> Result<Vec<u8>, char> {
    match encoding {
        Encoding::Utf8 => Ok(text.as_bytes().to_vec()),
        Encoding::Utf8Bom => Ok([&[0xEF, 0xBB, 0xBF], text.as_bytes()].concat()),
        Encoding::Utf16Le | Encoding::Utf16Be => {
            let little = encoding == Encoding::Utf16Le;
            let mut bytes = Vec::with_capacity(text.len() * 2 + 2);
            for unit in std::iter::once(0xFEFF).chain(text.encode_utf16()) {
                bytes.extend(match little {
                    true => unit.to_le_bytes(),
                    false => unit.to_be_bytes(),
                });
            }
            Ok(bytes)
        }
        Encoding::Windows1251 | Encoding::Windows1252 => text
            .chars()
            .map(|c| to_one_byte(c, encoding).ok_or(c))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_byte_of_a_single_byte_encoding_reads_and_writes_back() {
        let all: Vec<u8> = (0..=255).collect();
        for encoding in [Encoding::Windows1251, Encoding::Windows1252] {
            let text = decode_as(&all, encoding);
            assert_eq!(text.chars().count(), 256);
            assert_eq!(encode(&text, encoding).as_deref(), Ok(all.as_slice()));
        }
        assert_eq!(
            decode_as(
                &[0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2, 0xA8, 0xB9],
                Encoding::Windows1251
            ),
            "ПриветЁ№"
        );
        assert_eq!(
            decode_as(&[0x63, 0x61, 0x66, 0xE9, 0x80, 0x99], Encoding::Windows1252),
            "café€™"
        );
    }

    #[test]
    fn what_a_file_is_written_in_is_found_out() {
        let read = |bytes: &[u8]| {
            let decoded = decode(bytes);
            (decoded.text, decoded.encoding, decoded.binary)
        };
        assert_eq!(read(b"plain"), ("plain".into(), Encoding::Utf8, false));
        assert_eq!(
            read("привет".as_bytes()),
            ("привет".into(), Encoding::Utf8, false)
        );
        assert_eq!(
            read(b"\xEF\xBB\xBFmarked"),
            ("marked".into(), Encoding::Utf8Bom, false)
        );
        assert_eq!(
            read(b"\xFF\xFEh\0i\0"),
            ("hi".into(), Encoding::Utf16Le, false)
        );
        assert_eq!(
            read(b"\xFE\xFF\0h\0i"),
            ("hi".into(), Encoding::Utf16Be, false)
        );
        // Not UTF-8: words of letters past ASCII are Cyrillic, single
        // such letters among ASCII ones are Western.
        let russian = encode("// Привет, мир: это тест\n", Encoding::Windows1251).unwrap();
        assert_eq!(read(&russian).1, Encoding::Windows1251);
        assert_eq!(read(&russian).0, "// Привет, мир: это тест\n");
        let french = encode("// un café très naïf, déjà vu\n", Encoding::Windows1252).unwrap();
        assert_eq!(
            read(&french),
            (
                "// un café très naïf, déjà vu\n".into(),
                Encoding::Windows1252,
                false
            )
        );
        // A byte no text has: every byte is still read, to be shown.
        let (text, encoding, binary) = read(b"\x7FELF\0\x01\xFF");
        assert_eq!(
            (text.chars().count(), encoding, binary),
            (7, Encoding::Windows1252, true)
        );
        assert_eq!(read(b""), (String::new(), Encoding::Utf8, false));
    }

    #[test]
    fn a_text_is_written_as_it_was_read() {
        for encoding in Encoding::ALL {
            let text = match encoding {
                Encoding::Windows1251 => "строка one\n",
                Encoding::Windows1252 => "ligne été\n",
                _ => "строка 行 😀\n",
            };
            let bytes = encode(text, encoding).unwrap();
            let decoded = decode(&bytes);
            assert_eq!(decoded.text, text, "{}", encoding.name());
            // UTF-8 without a mark is told from nothing but itself.
            assert_eq!(decoded.encoding, encoding, "{}", encoding.name());
            assert_eq!(encode(&decoded.text, decoded.encoding).unwrap(), bytes);
        }
        // A character an encoding has no byte for is said, not replaced.
        assert_eq!(encode("naïve", Encoding::Windows1251), Err('ï'));
        assert_eq!(encode("да 😀", Encoding::Windows1252), Err('д'));
        // Half a unit of UTF-16 reads as no character, and is not lost
        // silently as a letter.
        assert_eq!(decode_as(b"\xFF\xFEh\0i", Encoding::Utf16Le), "h\u{FFFD}");
    }
}
