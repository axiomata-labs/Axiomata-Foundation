//! A plugin's page, checked before the app draws it: its size, its tree, its pictures. What a plugin hands the host
//! in one call is bounded before it is copied — wasmtime counts every byte against the store's hostcall fuel
//! ([`MAX_TRANSFER_BYTES`] in `host.rs`), also where a thousand strings of a page point at the same buffer — so the
//! page checked here is at most that large.

use crate::bindings::axiomata::plugin::ui;

/// How many blocks a page has, and how deep they nest.
const MAX_NODES: usize = 1000;
const MAX_DEPTH: usize = 8;
/// One text, and all texts of a page together, in bytes of UTF-8.
const MAX_TEXT_BYTES: usize = 10_000;
const MAX_PAGE_TEXT_BYTES: usize = 1024 * 1024;
/// One picture, all pictures of a page, and how many there are.
const MAX_PICTURE_BYTES: usize = 4 * 1024 * 1024;
const MAX_PAGE_PICTURE_BYTES: usize = 8 * 1024 * 1024;
const MAX_PICTURES: usize = 16;
/// A picture's width and height, in pixels: what it decodes to stays below 16 MiB.
const MAX_PICTURE_SIDE: u32 = 2048;
/// The pixels of all pictures of a page together: decoded, they stay below 32 MiB.
const MAX_PAGE_PIXELS: u64 = 8 * 1024 * 1024;
/// A choice's options.
const MAX_OPTIONS: usize = 200;

/// Whether the blocks fit together: every parent before its children and able to hold them, not too deep; every
/// picture a PNG or JPEG of a size the app may decode; every choice chose one of its options.
pub(crate) fn check(page: &ui::Page) -> Result<(), String> {
    if page.nodes.len() > MAX_NODES {
        return Err(format!("a page has at most {MAX_NODES} blocks"));
    }
    let (mut text_bytes, mut picture_bytes, mut pictures) = (0usize, 0usize, 0usize);
    let mut pixels = 0u64;
    let mut depth = vec![0usize; page.nodes.len()];
    for (index, node) in page.nodes.iter().enumerate() {
        for text in texts(&node.block) {
            if text.len() > MAX_TEXT_BYTES {
                return Err(format!(
                    "block {index} holds a text longer than {MAX_TEXT_BYTES} bytes"
                ));
            }
            text_bytes += text.len();
        }
        if text_bytes > MAX_PAGE_TEXT_BYTES {
            return Err(format!(
                "the page's texts are longer than {MAX_PAGE_TEXT_BYTES} bytes"
            ));
        }
        if let ui::Block::Picture(picture) = &node.block {
            pictures += 1;
            picture_bytes += picture.bytes.len();
            if picture.bytes.len() > MAX_PICTURE_BYTES
                || picture_bytes > MAX_PAGE_PICTURE_BYTES
                || pictures > MAX_PICTURES
            {
                return Err(format!(
                    "a page has at most {MAX_PICTURES} pictures of at most 4 MiB each and 8 MiB together"
                ));
            }
        }
        if let Some(parent) = node.parent {
            let parent = parent as usize;
            if parent >= index {
                return Err(format!(
                    "block {index} names a parent that does not come before it"
                ));
            }
            if !matches!(
                page.nodes[parent].block,
                ui::Block::Card(_) | ui::Block::Row | ui::Block::List
            ) {
                return Err(format!("block {index} sits in a block that holds none"));
            }
            depth[index] = depth[parent] + 1;
            if depth[index] >= MAX_DEPTH {
                return Err(format!("blocks nest at most {MAX_DEPTH} deep"));
            }
        }
        match &node.block {
            ui::Block::Picture(picture) => match dimensions(&picture.bytes) {
                Some((width, height))
                    if width <= MAX_PICTURE_SIDE && height <= MAX_PICTURE_SIDE =>
                {
                    pixels += u64::from(width) * u64::from(height);
                    if pixels > MAX_PAGE_PIXELS {
                        return Err(format!(
                            "the page's pictures hold more than {MAX_PAGE_PIXELS} pixels"
                        ));
                    }
                }
                Some(_) => {
                    return Err(format!(
                        "picture {index} is larger than {MAX_PICTURE_SIDE} pixels a side"
                    ));
                }
                None => return Err(format!("picture {index} is no PNG or JPEG")),
            },
            ui::Block::Choice(choice)
                if choice.options.len() > MAX_OPTIONS
                    || choice
                        .chosen
                        .is_some_and(|chosen| chosen as usize >= choice.options.len()) =>
            {
                return Err(format!(
                    "choice {index} has too many options or chose none of them"
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

/// Every text of `block`, its ids included.
fn texts(block: &ui::Block) -> Vec<&str> {
    match block {
        ui::Block::Card(text) | ui::Block::Text(text) | ui::Block::Note(text) => vec![text],
        ui::Block::Row | ui::Block::List => Vec::new(),
        ui::Block::Picture(picture) => vec![&picture.description],
        ui::Block::Button(button) => vec![&button.id, &button.label],
        ui::Block::Switch(switch) => vec![&switch.id, &switch.label],
        ui::Block::Field(field) => vec![&field.id, &field.label, &field.value, &field.placeholder],
        ui::Block::Choice(choice) => [&choice.id, &choice.label]
            .into_iter()
            .chain(&choice.options)
            .map(String::as_str)
            .collect(),
    }
}

/// A PNG's or JPEG's width and height from its header, without decoding it.
pub(crate) fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let be32 = |at: usize| {
        bytes
            .get(at..at + 4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    };
    let be16 = |at: usize| {
        bytes
            .get(at..at + 2)
            .map(|b| u32::from(u16::from_be_bytes([b[0], b[1]])))
    };
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.get(12..16) == Some(b"IHDR") {
        return Some((be32(16)?, be32(20)?));
    }
    if !bytes.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    // The segments up to the first frame header (SOF0–SOF15 but DHT, JPG and DAC), which holds the size.
    let mut at = 2;
    while at + 4 <= bytes.len() {
        if bytes[at] != 0xFF {
            return None;
        }
        let marker = bytes[at + 1];
        if marker == 0xFF {
            at += 1;
            continue;
        }
        let length = be16(at + 2)? as usize;
        if (0xC0..=0xCF).contains(&marker) && ![0xC4, 0xC8, 0xCC].contains(&marker) {
            return Some((be16(at + 7)?, be16(at + 5)?));
        }
        at += 2 + length;
    }
    None
}
