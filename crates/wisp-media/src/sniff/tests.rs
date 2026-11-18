use super::*;

#[test]
fn formats_are_recognised_by_their_magic_bytes() {
    let mut webp = b"RIFF\0\0\0\0WEBPVP8 ".to_vec();
    webp.extend([0; 8]);
    let mut bmp = b"BM".to_vec();
    bmp.extend([0; 40]);
    let cases: [(&[u8], Option<SourceFormat>); 12] = [
        (b"\x89PNG\r\n\x1a\nrest", Some(SourceFormat::Png)),
        (b"\xff\xd8\xff\xe0", Some(SourceFormat::Jpeg)),
        (b"GIF87a....", Some(SourceFormat::Gif)),
        (b"GIF89a....", Some(SourceFormat::Gif)),
        (&webp, Some(SourceFormat::Webp)),
        (&bmp, Some(SourceFormat::Bmp)),
        (b"\x89PNG\r\n", None),
        (b"RIFF\0\0\0\0WAVEfmt ", None),
        (b"BM", None),
        (b"<svg xmlns=", None),
        (b"II*\0", None),
        (b"", None),
    ];
    for (bytes, expected) in cases {
        assert_eq!(sniff(bytes), expected, "{bytes:?}");
    }
}
