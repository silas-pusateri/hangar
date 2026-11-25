# Hangar port spec: Rust stack probe (macOS arm64, rustc 1.97.0)

Probe project: `scratchpad/probe` (throwaway). Date: 2026-11-24. Host: 14 cores (10 P + 4 E). Host load average was 7-10 during all timings (other sessions busy), so treat timings as upper bounds.

## 1. Cargo.toml (all deps added with `cargo add`, no version guessing)

```toml
[dependencies]
eframe = { version = "0.37.0", default-features = false, features = ["glow", "default_fonts"] }
egui_extras = { version = "0.37.0", default-features = false }
flate2 = "1.1.11"
regex = "1.13.2"
serde = { version = "1.0.229", features = ["derive"] }
serde_json = "1.0.154"
sha1 = "0.11.0"
sha2 = "0.11.1"
tar = "0.4.46"
ureq = { version = "3.5.2", default-features = false, features = ["rustls"] }
zip = { version = "9.8.2", default-features = false, features = ["deflate"] }

[profile.release]
opt-level = 3
lto = "fat"
codegen-units = 1
strip = true
panic = "abort"
```

## 2. Resolved versions (Cargo.lock, 268 packages)

| crate | version | crate | version |
|---|---|---|---|
| eframe / egui / egui_extras | 0.37.0 | egui-winit / egui_glow / epaint / emath | 0.37.0 |
| winit | 0.30.13 | glow | 0.17.0 |
| glutin / glutin-winit | 0.32.3 / 0.5.0 | raw-window-handle | 0.6.2 |
| ureq / ureq-proto | 3.5.2 / 0.7.1 | rustls | 0.23.46 |
| ring | 0.17.14 | webpki-roots | 1.0.12 |
| serde / serde_core / serde_derive | 1.0.229 | serde_json | 1.0.154 |
| flate2 | 1.1.11 (backend zlib-rs 0.6.9) | miniz_oxide | 0.8.9 + 0.9.1 (via png/image in eframe) |
| tar | 0.4.46 | zip | 9.8.2 (+ zopfli 0.8.4) |
| sha1 / sha2 / digest | 0.11.0 / 0.11.1 / 0.11.3 | regex | 1.13.2 |
| objc2 | 0.5.3 + 0.6.5 (dup) | image | 0.25.10 (eframe icon) |

## 3. Build results

- `cargo check` (cold): 17 s wall. `cargo build --release` (cold, fat LTO): 88 s wall, 280 s CPU. Incremental release rebuild after a main.rs edit: ~49 s (fat LTO re-links everything). Use the dev profile for the edit loop.
- `cargo clippy`: 0 warnings for the code in section 7.
- Release binary: **6,500,752 bytes (6.2 MiB)**, Mach-O arm64, stripped. Links only system frameworks (OpenGL, AppKit, Foundation, CoreFoundation, CoreGraphics, CoreVideo, Carbon, ColorSync, CoreServices, ApplicationServices, libobjc, libiconv, libSystem). No non-system dylibs.
- Size of optional eframe features (measured): `+clipboard` = 6,881,888 B (+381 KB); `+clipboard,+accesskit` = 7,102,768 B (+221 KB more).

## 4. Runtime: GUI window (30,000-row virtual table + 1 worker thread)

Launched `target/release/probe` in background, sampled `ps -o rss=,%cpu=,time= -p PID` each 1 s, then `kill PID` (that pid only).

| t | RSS | %CPU | cumulative CPU |
|---|---|---|---|
| 1 s | 111,936 KB | 16.7 (startup) | 0:00.20 |
| 2 s | 111,936 KB | 0.4 | 0:00.21 |
| 3-6 s | 111,936 KB | **0.0** | 0:00.21 (no change) |

- `footprint PID`: **phys_footprint 52 MB**, phys_footprint_peak 169 MB (peak is at GL/font-atlas startup). RSS (109 MiB) includes shared framework pages; use phys_footprint as the real number.
- 4 OS threads at idle (`ps -M`; the worker had already exited).
- **Reactive mode confirmed.** Frame log (stderr, one line per `App::ui` call) shows 16 frames in 6 s: 13 at startup/window-show, 3 caused by the worker's `ctx.request_repaint()`, then 0 frames while idle. Cumulative CPU did not grow after 2 s. No `request_repaint_after` or animation is needed for idle = 0 % CPU. Do not call `request_repaint()` every frame; call it only from the worker when state changes.
- Pattern that works: clone `cc.egui_ctx` (it is `Send + Sync + Clone`) into the `std::thread`, write state into `Arc<Mutex<_>>`, then call `ctx.request_repaint()`.
- UNVERIFIED: scroll smoothness and frame time while scrolling the 30k table (not measured; `TableBody::rows` lays out only visible rows, so cost is O(visible)).

## 5. Metadata load time (31,445 `.ckan` files, 51,873,601 bytes, 4118 mods)

Std threads only (`std::thread::scope`, files split into equal chunks). Parse target `serde_json::Value` unless noted.

**A. From the directory tree (one `open/read` per file)**

| threads | walk | read + parse |
|---|---|---|
| 1 | 130-160 ms | 1003-1340 ms |
| 2 | 135 ms | 601 ms |
| 4 | 141-146 ms | 448-500 ms |
| 6 | 131 ms | 467 ms |
| 8 | 133 ms | 657 ms |
| 14 | 141-212 ms | 1963-2131 ms (worse than 1 thread) |
| 1, typed struct `{identifier, version, name}` | 136 ms | 771 ms |

Per-file open/read cost dominates. More than ~4 threads makes it slower (filesystem contention on APFS; UNVERIFIED whether endpoint-security scanning adds to it).

**B. From `master.tar.gz` (5,389,433 B; one sequential stream through `GzDecoder` + `tar::Archive`, blobs into `Vec<Vec<u8>>`, then parse on N threads)**

| threads | gunzip + untar | parse |
|---|---|---|
| 1 | 103 ms | 155 ms |
| 2 | 100 ms | 141 ms |
| 4 | 101 ms | 60 ms |
| 8 | 101 ms | 40 ms |
| 14 | 100 ms | 35 ms |

All 31,445 files parse without error as `serde_json::Value`.

**Rule for Hangar:** never keep the registry as 31k loose files. Read the downloaded `master.tar.gz` as one stream (about 0.14-0.26 s total cold, 4-8 threads), or keep a single cache file of your own. Use `std::thread::available_parallelism()` capped at 8 for the parse stage. Parsing straight to typed structs is ~25 % faster than to `Value` (771 vs 1003 ms single thread on the directory path); UNVERIFIED for the tar path.

## 6. API gotchas found (all verified by compile or by reading the crate source in `~/.cargo/registry/src`)

1. **eframe 0.37 `App` trait changed.** The required method is now `fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame)` (epi.rs). There is no `update(ctx, frame)`. Optional `fn logic(&mut self, ctx: &egui::Context, frame: &mut Frame)` runs before each `ui` and also while the window is hidden. `on_exit(&mut self, gl: Option<&glow::Context>)` with the `glow` feature.
2. **Panels take `&mut Ui`, not `&Context`.** `egui::Panel::top("id").show(ui, |ui| ..)`, `Panel::bottom(..)`; `egui::CentralPanel::default().show(ui, |ui| ..)`. `show_inside` is deprecated (renamed to `show`). `TopBottomPanel`/`SidePanel` names do not exist in egui 0.37 (grep: no match).
3. **`run_native` signature (verbatim):**
   `pub fn run_native(app_name: &str, native_options: NativeOptions, app_creator: AppCreator<'_>) -> Result`
   with `pub type AppCreator<'app> = Box<dyn 'app + FnOnce(&CreationContext<'_>) -> Result<Box<dyn 'app + App>, DynError>>;`
   Call: `eframe::run_native("Hangar", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))?;`
   Also exists: `run_ui_native(app_name, native_options, impl FnMut(&mut egui::Ui, &mut Frame) + 'static)`.
4. **`NativeOptions`:** `eframe::NativeOptions { viewport: egui::ViewportBuilder::default().with_title("..").with_inner_size([900.0, 600.0]), ..Default::default() }`. Other fields seen in epi.rs: `renderer`, `run_and_return`, `centered` (others UNVERIFIED).
5. **TableBuilder (egui_extras 0.37) verbatim:** `TableBuilder::new(ui: &'a mut Ui)`, `.striped(bool)`, `.column(Column)`, `.columns(Column, usize)`, `.header(height: f32, impl FnOnce(TableRow<'_, '_>)) -> Table<'a>`, `Table::body<F>(self, F) -> ScrollAreaOutput<()> where F: for<'b> FnOnce(TableBody<'b>)`, `TableBody::rows(self, row_height_sans_spacing: f32, total_rows: usize, impl FnMut(TableRow<'_, '_>))`, `TableRow::col(&mut self, impl FnOnce(&mut Ui)) -> (Rect, Response)`, `row.index()`. Columns: `Column::initial(w).resizable(true)`, `Column::remainder()`, `Column::exact(w)`, `Column::auto()`, `.at_least/.at_most/.clip`. Also `.sense(Sense)`, `.scroll_to_row(row, Option<Align>)`, `row.set_selected(bool)` for selection.
6. **`default-features = false` on eframe disables `clipboard` (arboard) and `accesskit`.** Without `clipboard`, Cmd+C/Cmd+V work only inside the app (fallback clipboard in egui-winit/src/clipboard.rs). Hangar needs paste in the search box: add `features = ["glow", "default_fonts", "clipboard"]` (+381 KB). `persistence` (ron + serde) is also off; Hangar must save its own settings.
7. **sha1/sha2 0.11 (digest 0.11, hybrid-array 0.4): `format!("{:x}", Sha256::digest(..))` does NOT compile** (`Array<u8, ..>: LowerHex` not satisfied, verified with a test crate). Write a 6-line hex helper (section 7 `hex()`). One `use sha1::Digest as _;` covers both sha1 and sha2 (same `digest` crate).
8. **ureq 3:** `Body::read_to_vec()` / `read_to_string()` / `read_json()` are **capped at 10 MB** (`MAX_BODY_SIZE`, body/mod.rs ~300). For mod zips use `resp.body_mut().with_config().limit(n).read_to_vec()` or, better, stream with `resp.into_body().into_reader()` (no limit) into a file while hashing. Agent: `let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(d)).user_agent("..").build().into();`. Defaults (config.rs ~1060): `http_status_as_error: true` (a 404 is `Err(ureq::Error::StatusCode(404))`), `max_redirects: 10`, proxy read from env, `https_only: false`. Set `timeout_global` explicitly (default timeouts UNVERIFIED). Feature `rustls` = ring + **bundled webpki-roots** (Mozilla list, not the macOS keychain); a `platform-verifier` feature exists for system trust (not tested).
9. **ureq `default-features = false, features = ["rustls"]` drops `gzip`.** CKAN metadata hosts serve `.tar.gz` files, so this is fine; enable `gzip` only if a server sends `Content-Encoding: gzip`.
10. **zip 9.8:** `ZipFile::name()` returns `ZipResult<Cow<'_, str>>` (use `f.name()?`); `ZipFile<'_, R>` is generic over the reader; `enclosed_name() -> Option<PathBuf>` rejects `..` and absolute paths (use it for install path safety); `crc32()`, `size()`, `is_dir()`. Writer: `zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated)`, `start_file(name, opts)`, `finish()? -> W`. Feature `deflate` = `deflate-zopfli` + `deflate-flate2-zlib-rs`. For read-only use `features = ["deflate-flate2-zlib-rs"]` to drop zopfli (size gain UNVERIFIED; LTO may strip it).
11. **flate2 backend unifies to zlib-rs** (pulled in by zip's `deflate-flate2-zlib-rs`). No C toolchain needed.
12. **`panic = "abort"`:** a panic in any worker thread kills the whole app; `JoinHandle::join()` cannot catch it. Worker code must return `Result` and send errors over the channel, never `unwrap()` on I/O or JSON.
13. **tar 0.4:** `tar::Archive::new(flate2::read::GzDecoder::new(BufReader<File>))`, `entries()?`, `e.path()?`, `e.size()`, `read_to_end`. Entries must be read in order (streaming); do not keep `Entry` values.

## 7. Full working `src/main.rs`

Modes: no args = GUI; `parse <dir> [threads] [typed]`; `tgz <master.tar.gz> [threads]`; `selftest` (regex, sha1, sha256, tar.gz and zip round trip); `fetch <url>` (compiled, not run: no network use in this probe). `selftest` output: `sha1=a9993e36...d89d`, `sha256=ba7816bf...15ad` (correct known vectors for "abc").

```rust
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use egui_extras::{Column, TableBuilder};

#[derive(serde::Deserialize)]
struct Mini {
    identifier: String,
    version: String,
    #[serde(default)]
    name: Option<String>,
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        match e.file_type() {
            Ok(t) if t.is_dir() => walk(&p, out),
            Ok(t) if t.is_file() && p.extension().is_some_and(|x| x == "ckan") => out.push(p),
            _ => {}
        }
    }
}

fn parse(dir: &str, threads: usize, typed: bool) {
    let t0 = Instant::now();
    let mut files = Vec::new();
    walk(Path::new(dir), &mut files);
    let t_walk = t0.elapsed();
    let t1 = Instant::now();
    let chunk = files.len().div_ceil(threads.max(1)).max(1);
    let (ok, bad, bytes) = std::thread::scope(|s| {
        let hs: Vec<_> = files
            .chunks(chunk)
            .map(|c| {
                s.spawn(move || {
                    let (mut ok, mut bad, mut bytes) = (0usize, 0usize, 0usize);
                    for p in c {
                        let Ok(buf) = std::fs::read(p) else { bad += 1; continue };
                        bytes += buf.len();
                        let r = if typed {
                            serde_json::from_slice::<Mini>(&buf).map(|m| {
                                std::hint::black_box((m.identifier.len(), m.version.len(), m.name.is_some()));
                            })
                        } else {
                            serde_json::from_slice::<serde_json::Value>(&buf).map(|v| {
                                std::hint::black_box(v);
                            })
                        };
                        if r.is_ok() { ok += 1 } else { bad += 1 }
                    }
                    (ok, bad, bytes)
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).fold((0, 0, 0), |a, b| (a.0 + b.0, a.1 + b.1, a.2 + b.2))
    });
    println!(
        "files={} ok={ok} bad={bad} bytes={bytes} threads={threads} typed={typed} walk={:.1}ms parse={:.1}ms",
        files.len(),
        t_walk.as_secs_f64() * 1e3,
        t1.elapsed().as_secs_f64() * 1e3
    );
}

// Read master.tar.gz into memory (one sequential stream), then parse the .ckan blobs on N threads.
fn parse_tgz(path: &str, threads: usize) -> Result<(), Box<dyn std::error::Error>> {
    let t0 = Instant::now();
    let f = std::io::BufReader::new(std::fs::File::open(path)?);
    let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(f));
    let mut blobs: Vec<Vec<u8>> = Vec::with_capacity(32_000);
    for e in ar.entries()? {
        let mut e = e?;
        if e.path()?.extension().is_some_and(|x| x == "ckan") {
            let mut v = Vec::with_capacity(e.size() as usize);
            e.read_to_end(&mut v)?;
            blobs.push(v);
        }
    }
    let t_unpack = t0.elapsed();
    let t1 = Instant::now();
    let chunk = blobs.len().div_ceil(threads.max(1)).max(1);
    let ok: usize = std::thread::scope(|s| {
        let hs: Vec<_> = blobs
            .chunks(chunk)
            .map(|c| s.spawn(move || c.iter().filter(|b| serde_json::from_slice::<serde_json::Value>(b).is_ok()).count()))
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).sum()
    });
    println!(
        "tgz blobs={} ok={ok} threads={threads} gunzip+untar={:.1}ms parse={:.1}ms",
        blobs.len(),
        t_unpack.as_secs_f64() * 1e3,
        t1.elapsed().as_secs_f64() * 1e3
    );
    Ok(())
}

fn selftest() -> Result<(), Box<dyn std::error::Error>> {
    use sha1::Digest as _;
    let re = regex::Regex::new(r"^[A-Za-z0-9][A-Za-z0-9-]+$")?;
    println!("regex ok={}", re.is_match("ModuleManager"));
    println!("sha1={}", hex(&sha1::Sha1::digest(b"abc")));
    println!("sha256={}", hex(&sha2::Sha256::digest(b"abc")));

    let mut tb = tar::Builder::new(Vec::new());
    let data = b"hello hangar";
    let mut h = tar::Header::new_gnu();
    h.set_size(data.len() as u64);
    h.set_mode(0o644);
    h.set_cksum();
    tb.append_data(&mut h, "GameData/x.txt", &data[..])?;
    let tar_bytes = tb.into_inner()?;
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(&tar_bytes)?;
    let tgz = gz.finish()?;
    let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(&tgz[..]));
    for e in ar.entries()? {
        let mut e = e?;
        let mut s = String::new();
        e.read_to_string(&mut s)?;
        println!("tar.gz entry {} = {s}", e.path()?.display());
    }

    let mut zw = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    zw.start_file("GameData/Mod/a.cfg", opts)?;
    zw.write_all(b"PART {}")?;
    let zbytes = zw.finish()?.into_inner();
    let mut za = zip::ZipArchive::new(Cursor::new(zbytes))?;
    for i in 0..za.len() {
        let mut f = za.by_index(i)?;
        let mut s = String::new();
        f.read_to_string(&mut s)?;
        println!("zip entry {} enclosed={:?} crc={:08x} = {s}", f.name()?, f.enclosed_name(), f.crc32());
    }
    Ok(())
}

fn fetch(url: &str) -> Result<(), Box<dyn std::error::Error>> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .user_agent("hangar-probe/0.1")
        .build()
        .into();
    let mut resp = agent.get(url).call()?;
    let bytes = resp.body_mut().with_config().limit(1 << 30).read_to_vec()?;
    println!("status={} bytes={}", resp.status(), bytes.len());
    Ok(())
}

struct Row {
    ident: String,
    name: String,
    version: String,
}

struct ProbeApp {
    rows: Vec<Row>,
    status: Arc<Mutex<String>>,
    frames: u64,
}

impl ProbeApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let rows = (0..30_000)
            .map(|i| Row { ident: format!("Mod{i:05}"), name: format!("Example Mod Number {i}"), version: format!("1.{}.{}", i % 17, i % 5) })
            .collect();
        let status = Arc::new(Mutex::new("worker: starting".to_owned()));
        let ctx = cc.egui_ctx.clone();
        let st = Arc::clone(&status);
        std::thread::spawn(move || {
            for i in 1..=3 {
                std::thread::sleep(Duration::from_millis(300));
                *st.lock().unwrap() = format!("worker: step {i}/3");
                ctx.request_repaint();
            }
            *st.lock().unwrap() = "worker: done".to_owned();
            ctx.request_repaint();
        });
        Self { rows, status, frames: 0 }
    }
}

impl eframe::App for ProbeApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.frames += 1;
        let status = self.status.lock().unwrap().clone();
        eprintln!("frame {} {status}", self.frames);
        egui::Panel::top("top").show(ui, |ui| {
            ui.label(format!("{status} | frames={}", self.frames));
        });
        egui::CentralPanel::default().show(ui, |ui| {
            TableBuilder::new(ui)
                .striped(true)
                .column(Column::initial(140.0).resizable(true))
                .column(Column::remainder())
                .column(Column::exact(80.0))
                .header(20.0, |mut h| {
                    h.col(|ui| { ui.strong("Identifier"); });
                    h.col(|ui| { ui.strong("Name"); });
                    h.col(|ui| { ui.strong("Version"); });
                })
                .body(|body| {
                    body.rows(18.0, self.rows.len(), |mut row| {
                        let r = &self.rows[row.index()];
                        row.col(|ui| { ui.label(&r.ident); });
                        row.col(|ui| { ui.label(&r.name); });
                        row.col(|ui| { ui.label(&r.version); });
                    });
                });
        });
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("parse") => {
            let n = args.get(3).and_then(|s| s.parse().ok()).unwrap_or_else(|| std::thread::available_parallelism().map_or(4, |n| n.get()));
            parse(&args[2], n, args.get(4).is_some_and(|s| s == "typed"));
            Ok(())
        }
        Some("tgz") => parse_tgz(&args[2], args.get(3).and_then(|s| s.parse().ok()).unwrap_or(4)),
        Some("selftest") => selftest(),
        Some("fetch") => fetch(&args[2]),
        _ => {
            let options = eframe::NativeOptions {
                viewport: egui::ViewportBuilder::default().with_title("Hangar probe").with_inner_size([900.0, 600.0]),
                ..Default::default()
            };
            eframe::run_native("Hangar probe", options, Box::new(|cc| Ok(Box::new(ProbeApp::new(cc)))))?;
            Ok(())
        }
    }
}
```
