//! Import any interchange document (EDL, FCP7 XML, FCPXML, OTIO, AAF, OMF, Premiere .prproj) and
//! print what came across: bins, media, sequences, tracks and clips.
//!
//!     cargo run -p filmcraft-interchange --example check_import -- path/to/project.prproj
use filmcraft_interchange::{Format, detect, import};
use filmcraft_project::{BinEntry, TrackKind};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("usage: check_import <file>")?;
    let bytes = std::fs::read(&path)?;
    let p = std::path::Path::new(&path);
    let ext = p.extension().and_then(|e| e.to_str());
    let format = detect(&bytes, ext).or_else(|| ext.and_then(Format::from_extension)).ok_or("unknown format")?;
    let base = p.parent().and_then(|d| d.to_str()).map(str::to_string);
    let (imp, report) = import(&bytes, format, base.as_deref())?;
    println!("format: {}", format.name());
    fn walk(e: &[BinEntry], depth: usize, p: &filmcraft_project::Project) {
        for c in e {
            match c {
                BinEntry::Bin(b) => {
                    println!("{}[bin] {}", "  ".repeat(depth), b.name);
                    walk(&b.children, depth + 1, p);
                }
                BinEntry::Item(id) => {
                    if let Some(it) = p.item(*id) {
                        println!("{}- {}", "  ".repeat(depth), it.name);
                    }
                }
            }
        }
    }
    walk(&imp.project.root.children, 0, &imp.project);
    for sid in &imp.sequences {
        let (Some(it), Some(s)) = (imp.project.item(*sid), imp.project.sequence(*sid)) else { continue };
        println!(
            "\nsequence \"{}\": {}x{} @ {}/{} fps, {} Hz",
            it.name, s.settings.width, s.settings.height, s.settings.frame_rate.num, s.settings.frame_rate.den, s.settings.sample_rate
        );
        for kind in [TrackKind::Video, TrackKind::Audio] {
            for (i, t) in s.tracks(kind).iter().enumerate() {
                if t.items.is_empty() {
                    continue;
                }
                println!("  {:?} {} ({} clips){}", kind, i + 1, t.items.len(), if t.muted { " muted" } else { "" });
                for c in &t.items {
                    let src = imp.project.item(c.item).map(|i| i.name.as_str()).unwrap_or("?");
                    println!(
                        "    {:>8.3}s +{:>7.3}s  in {:>8.3}s  link {:?}  {}  <- {}",
                        c.start.0 as f64 / 254_016_000_000.0,
                        c.duration.0 as f64 / 254_016_000_000.0,
                        c.source_in.0 as f64 / 254_016_000_000.0,
                        c.link,
                        c.name,
                        src
                    );
                }
            }
        }
    }
    println!("\nreport: {:?}", report);
    Ok(())
}
