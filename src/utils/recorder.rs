use std::fs::{self, File};
use std::io::{self, BufWriter};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::{ExtendedColorType, ImageEncoder};
use winisland_render::RecordedFrame;

pub(crate) const RECORDING_FPS: u32 = 60;
const ENCODERS: usize = 3;
const IMPORT_SCRIPT: &str = "导入到AE.jsx";

struct Job {
    first: u64,
    last: u64,
    frame: RecordedFrame,
}

pub(crate) struct Recorder {
    directory: PathBuf,
    worker: JoinHandle<io::Result<u64>>,
}

impl Recorder {
    pub(crate) fn start() -> io::Result<(Self, Sender<RecordedFrame>)> {
        let time = crate::platform::shell().local_datetime();
        let directory = dirs::video_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("WinIsland")
            .join(format!(
                "WinIsland {:04}-{:02}-{:02} {:02}-{:02}-{:02}",
                time.year, time.month, time.day, time.hour, time.minute, time.second
            ));
        fs::create_dir_all(&directory)?;
        let (sender, receiver) = mpsc::channel();
        let target = directory.clone();
        let worker = thread::Builder::new()
            .name("island-recorder".to_string())
            .spawn(move || schedule(&target, receiver))?;
        Ok((Self { directory, worker }, sender))
    }

    /// Waits for every frame to be written, adds the After Effects import script, and returns
    /// the folder with the number of frames; the frame sender must already be dropped.
    pub(crate) fn finish(self) -> io::Result<(PathBuf, u64)> {
        let frames = self
            .worker
            .join()
            .map_err(|_| io::Error::other("recorder thread panicked"))??;
        if frames > 0 {
            fs::write(self.directory.join(IMPORT_SCRIPT), import_script())?;
        }
        Ok((self.directory, frames))
    }
}

fn schedule(directory: &Path, receiver: Receiver<RecordedFrame>) -> io::Result<u64> {
    let (jobs, queue) = mpsc::channel::<Job>();
    let queue = Arc::new(Mutex::new(queue));
    let encoders: Vec<JoinHandle<io::Result<()>>> = (0..ENCODERS)
        .map(|_| {
            let queue = Arc::clone(&queue);
            let directory = directory.to_path_buf();
            thread::spawn(move || {
                loop {
                    let job = queue
                        .lock()
                        .map_err(|_| io::Error::other("queue poisoned"))?
                        .recv();
                    match job {
                        Ok(job) => write_job(&directory, job)?,
                        Err(_) => return Ok(()),
                    }
                }
            })
        })
        .collect();

    let mut start: Option<Instant> = None;
    let mut pending: Option<(u64, RecordedFrame)> = None;
    for frame in receiver {
        let origin = *start.get_or_insert(frame.at);
        let index = (frame.at.saturating_duration_since(origin).as_secs_f64()
            * f64::from(RECORDING_FPS))
        .round() as u64;
        pending = match pending.take() {
            Some((first, held)) if index > first => {
                let _ = jobs.send(Job {
                    first,
                    last: index - 1,
                    frame: held,
                });
                Some((index, frame))
            }
            Some((first, _)) => Some((first, frame)),
            None => Some((0, frame)),
        };
    }
    let total = match pending {
        Some((first, frame)) => {
            let _ = jobs.send(Job {
                first,
                last: first,
                frame,
            });
            first + 1
        }
        None => 0,
    };
    drop(jobs);
    for encoder in encoders {
        encoder
            .join()
            .map_err(|_| io::Error::other("encoder thread panicked"))??;
    }
    Ok(total)
}

fn frame_path(directory: &Path, index: u64) -> PathBuf {
    directory.join(format!("island_{index:05}.png"))
}

fn write_job(directory: &Path, job: Job) -> io::Result<()> {
    let Job { first, last, frame } = job;
    let rgb: Vec<u8> = frame
        .pixels
        .chunks_exact(4)
        .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
        .collect();
    let path = frame_path(directory, first);
    PngEncoder::new_with_quality(
        BufWriter::new(File::create(&path)?),
        CompressionType::Fast,
        FilterType::Sub,
    )
    .write_image(&rgb, frame.width, frame.height, ExtendedColorType::Rgb8)
    .map_err(io::Error::other)?;
    for index in first + 1..=last {
        let copy = frame_path(directory, index);
        if fs::hard_link(&path, &copy).is_err() {
            fs::copy(&path, &copy)?;
        }
    }
    Ok(())
}

fn import_script() -> String {
    format!(
        r#"(function () {{
    var folder = File($.fileName).parent;
    var options = new ImportOptions(new File(folder.fsName + "/island_00000.png"));
    options.sequence = true;
    options.forceAlphabetical = true;
    app.beginUndoGroup("Import WinIsland recording");
    var footage = app.project.importFile(options);
    footage.mainSource.conformFrameRate = {fps};
    var comp = app.project.items.addComp(decodeURI(folder.name), footage.width, footage.height, 1, footage.duration, {fps});
    comp.layers.add(footage);
    comp.openInViewer();
    app.endUndoGroup();
}})();
"#,
        fps = RECORDING_FPS
    )
}
