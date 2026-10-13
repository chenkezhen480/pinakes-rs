//! Reads one PDF from stdin and writes to stdout:
//!
//! - `pinakes-pdf text <max decoded bytes> <max markdown bytes>`: its text layer as Markdown;
//! - `pinakes-pdf pages`: its number of pages;
//! - `pinakes-pdf page <n>`: page `n` (from 1) drawn as a PNG, for the model to read a scan.
//!
//! The service runs it in a process of its own for every PDF and page (`convert::pdf`), so a
//! crafted file that would take gigabytes, crash the reader or never end costs only this
//! process: its allocator refuses past [`pdf::MEMORY`] (the process then stops), and the service
//! kills it past its deadline. Exit codes: 0 with the answer, or `EXIT_UNREADABLE`,
//! `EXIT_NO_TEXT`, `EXIT_TOO_LARGE` with the reason on stderr.

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};

use pinakes::convert::{Limits, Refused, pdf};

/// The system allocator, refusing once [`pdf::MEMORY`] bytes are in use.
struct Capped;

static IN_USE: AtomicUsize = AtomicUsize::new(0);

impl Capped {
    fn take(size: usize) -> bool {
        let before = IN_USE.fetch_add(size, Ordering::Relaxed);
        if before.saturating_add(size) > pdf::MEMORY {
            IN_USE.fetch_sub(size, Ordering::Relaxed);
            return false;
        }
        true
    }

    fn give(size: usize) {
        IN_USE.fetch_sub(size, Ordering::Relaxed);
    }
}

// SAFETY: every call goes to the system allocator with the same layout; this only counts the
// bytes and answers null (an allocation failure) instead of going past the cap.
unsafe impl GlobalAlloc for Capped {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if !Self::take(layout.size()) {
            return std::ptr::null_mut();
        }
        // SAFETY: as the caller's contract for `alloc`.
        let p = unsafe { System.alloc(layout) };
        if p.is_null() {
            Self::give(layout.size());
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if !Self::take(layout.size()) {
            return std::ptr::null_mut();
        }
        // SAFETY: as the caller's contract for `alloc_zeroed`.
        let p = unsafe { System.alloc_zeroed(layout) };
        if p.is_null() {
            Self::give(layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: as the caller's contract for `dealloc`.
        unsafe { System.dealloc(ptr, layout) };
        Self::give(layout.size());
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let old = layout.size();
        if new_size > old && !Self::take(new_size - old) {
            return std::ptr::null_mut();
        }
        // SAFETY: as the caller's contract for `realloc`.
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if p.is_null() {
            if new_size > old {
                Self::give(new_size - old);
            }
        } else if new_size < old {
            Self::give(old - new_size);
        }
        p
    }
}

#[global_allocator]
static ALLOCATOR: Capped = Capped;

fn fail(code: i32, why: &str) -> ! {
    eprintln!("{why}");
    std::process::exit(code);
}

const USAGE: &str = "usage: pinakes-pdf text <max decoded bytes> <max markdown bytes> | pages | page <n>, with the PDF on stdin";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let number = |i: usize| args.get(i).and_then(|a| a.parse::<u64>().ok());
    let mut limits = Limits::default();
    let mut bytes = Vec::new();
    if std::io::stdin()
        .take(limits.bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .is_err()
    {
        fail(pdf::EXIT_UNREADABLE, "the PDF couldn't be read from stdin");
    }
    let answer = match (args.first().map(String::as_str), number(1), number(2)) {
        (Some("text"), Some(unpacked), Some(markdown_bytes)) => {
            limits.unpacked = unpacked;
            limits.markdown_bytes = usize::try_from(markdown_bytes).unwrap_or(usize::MAX);
            pdf::markdown(&bytes, &limits).map(String::into_bytes)
        }
        (Some("pages"), None, None) => pdf::count_pages(&bytes).map(|n| n.to_string().into_bytes()),
        (Some("page"), Some(n), None) => {
            pdf::page_png(&bytes, usize::try_from(n).unwrap_or(usize::MAX))
        }
        _ => fail(pdf::EXIT_UNREADABLE, USAGE),
    };
    match answer {
        Ok(answer) => {
            let mut out = std::io::stdout().lock();
            if out.write_all(&answer).and_then(|()| out.flush()).is_err() {
                std::process::exit(pdf::EXIT_UNREADABLE);
            }
        }
        Err(Refused::TooLarge(why)) => fail(pdf::EXIT_TOO_LARGE, &why),
        Err(Refused::NoText(why)) => fail(pdf::EXIT_NO_TEXT, &why),
        Err(Refused::Unreadable(why)) => fail(pdf::EXIT_UNREADABLE, &why),
    }
}
