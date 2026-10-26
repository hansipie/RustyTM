use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub const CATEGORIES: [&str; 5] = ["Media", "Documents", "Archives", "Applications", "Other"];

const MEDIA: &str = "png jpg jpeg gif webp bmp tiff heic svg mp3 flac wav ogg opus m4a aac mp4 mkv avi mov webm wmv m4v";
const DOCS: &str = "pdf doc docx txt rtf md odt xls xlsx csv tsv ods ppt pptx odp epub mobi tex";
const ARCH: &str = "zip tar gz bz2 xz 7z rar iso img jar war cab zst deb rpm";
const APPS: &str = "so a o AppImage exe dll py rs go c h cpp js ts java sh pl lua html css json xml yml yaml toml";

pub fn category(ext: &str) -> usize {
    let e = ext.to_lowercase();
    let has = |l: &str| l.split(' ').any(|x| x.eq_ignore_ascii_case(&e));
    if has(MEDIA) { 0 } else if has(DOCS) { 1 } else if has(ARCH) { 2 } else if has(APPS) { 3 } else { 4 }
}

#[derive(Default, Clone)]
pub struct ScanState {
    pub root: PathBuf,
    pub running: bool,
    pub paused: bool,
    pub done: bool,
    pub stopped: bool,
    pub files: u64,
    pub bytes: u64,
    pub unreadable: u64,
    pub current: String,
    pub by_cat: [u64; 5],
    pub by_ext: HashMap<String, (u64, u64)>,
    pub top_files: Vec<(u64, PathBuf)>,
    pub top_dirs: Vec<(u64, PathBuf)>,
}

pub struct Scanner {
    pub state: Arc<Mutex<ScanState>>,
    stop: Arc<AtomicBool>,
    pause: Arc<AtomicBool>,
}

impl Scanner {
    pub fn new() -> Self {
        Scanner { state: Arc::new(Mutex::new(ScanState::default())), stop: Arc::new(AtomicBool::new(false)), pause: Arc::new(AtomicBool::new(false)) }
    }

    pub fn snapshot(&self) -> ScanState {
        self.state.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn running(&self) -> bool {
        self.state.lock().map(|s| s.running).unwrap_or(false)
    }

    pub fn start(&mut self, root: PathBuf) {
        if self.running() {
            return;
        }
        self.stop.store(false, Ordering::SeqCst);
        self.pause.store(false, Ordering::SeqCst);
        *self.state.lock().unwrap() = ScanState { root: root.clone(), running: true, ..Default::default() };
        let (st, stop, pause) = (self.state.clone(), self.stop.clone(), self.pause.clone());
        std::thread::spawn(move || {
            let dev = std::fs::symlink_metadata(&root).map(|m| m.dev()).unwrap_or(0);
            let mut ctx = Ctx { st: st.clone(), stop: stop.clone(), pause, dev, local_files: Vec::new(), local_dirs: Vec::new() };
            ctx.walk(&root);
            ctx.flush();
            let mut s = st.lock().unwrap();
            s.running = false;
            s.done = !stop.load(Ordering::SeqCst);
            s.stopped = stop.load(Ordering::SeqCst);
            s.paused = false;
        });
    }

    pub fn toggle_pause(&mut self) {
        let p = !self.pause.load(Ordering::SeqCst);
        self.pause.store(p, Ordering::SeqCst);
        if let Ok(mut s) = self.state.lock() {
            s.paused = p;
        }
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.pause.store(false, Ordering::SeqCst);
    }
}

struct Ctx {
    st: Arc<Mutex<ScanState>>,
    stop: Arc<AtomicBool>,
    pause: Arc<AtomicBool>,
    dev: u64,
    local_files: Vec<(u64, PathBuf)>,
    local_dirs: Vec<(u64, PathBuf)>,
}

impl Ctx {
    fn flush(&mut self) {
        if let Ok(mut s) = self.st.lock() {
            s.top_files.append(&mut self.local_files);
            s.top_files.sort_by(|a, b| b.0.cmp(&a.0));
            s.top_files.truncate(100);
            s.top_dirs.append(&mut self.local_dirs);
            s.top_dirs.sort_by(|a, b| b.0.cmp(&a.0));
            s.top_dirs.truncate(100);
        }
    }

    fn walk(&mut self, dir: &Path) -> u64 {
        let mut total = 0;
        let Ok(rd) = std::fs::read_dir(dir) else {
            if let Ok(mut s) = self.st.lock() {
                s.unreadable += 1;
            }
            return 0;
        };
        let mut batch = (0u64, 0u64, [0u64; 5], Vec::<(String, u64)>::new());
        for e in rd.flatten() {
            while self.pause.load(Ordering::SeqCst) && !self.stop.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            if self.stop.load(Ordering::SeqCst) {
                break;
            }
            let p = e.path();
            let Ok(md) = std::fs::symlink_metadata(&p) else {
                if let Ok(mut s) = self.st.lock() {
                    s.unreadable += 1;
                }
                continue;
            };
            let ft = md.file_type();
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                if md.dev() != self.dev || ["/proc", "/sys", "/dev"].iter().any(|x| p == Path::new(x)) {
                    continue;
                }
                total += self.walk(&p);
            } else if ft.is_file() {
                let size = md.blocks() * 512; // allocated size, like du
                total += size;
                batch.0 += 1;
                batch.1 += size;
                let ext = p.extension().map(|x| x.to_string_lossy().into_owned()).unwrap_or_default();
                batch.2[category(&ext)] += size;
                batch.3.push((ext, size));
                if self.local_files.len() < 400 || size > 1 << 20 {
                    self.local_files.push((size, p));
                    if self.local_files.len() > 2000 {
                        self.local_files.sort_by(|a, b| b.0.cmp(&a.0));
                        self.local_files.truncate(100);
                    }
                }
            }
        }
        if let Ok(mut s) = self.st.lock() {
            s.files += batch.0;
            s.bytes += batch.1;
            for i in 0..5 {
                s.by_cat[i] += batch.2[i];
            }
            for (ext, sz) in batch.3 {
                let e = s.by_ext.entry(ext).or_default();
                e.0 += 1;
                e.1 += sz;
            }
            s.current = dir.to_string_lossy().into_owned();
        }
        self.local_dirs.push((total, dir.to_path_buf()));
        if self.local_dirs.len() > 4000 {
            self.flush();
        }
        total
    }
}
