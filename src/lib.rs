#![warn(missing_docs)]
//! A multi-threaded image compression tool, powered by [turbojpeg](https://github.com/honzasp/rust-turbojpeg).
use clap::Parser;
use colored::Colorize;
use crossbeam::deque::{Steal, Stealer, Worker};
use image::EncodableLayout;
use img_parts::{jpeg::Jpeg, ImageEXIF, ImageICC};
use std::env::current_dir;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::{fs::DirEntry, path::PathBuf};
use turbojpeg::{compress_image, decompress_image, Subsamp::Sub2x2};
#[derive(Parser, Debug)]
/// Get arguments from the terminal.
pub struct TaskArgs {
    /// Ranges from 1 (smallest file, worst quality) to 100 (biggest file, best quality).
    #[arg(default_value_t = 50)]
    quality: u8,
    /// The output directory of compressed images.
    #[arg(default_value_t = format!("compressed"))]
    output_dir: String,
    /// The number of worker threads used.
    #[arg(short, default_value_t = 4)]
    device: u8,
}
impl TaskArgs {
    /// Returns the quality after compression.
    pub fn get_quality(&self) -> i32 {
        self.quality.into()
    }
    /// Checks command-line input.
    pub fn verify(&self) {
        if self.quality < 1 || self.quality > 100 {
            eprintln!("Quality must be between 1 and 100");
            std::process::exit(1);
        }
    }
}
/// Obtain tasks from the current working directory.
pub struct Tasks {
    queue: Worker<Option<DirEntry>>,
    device_num: u8,
    output_dir: PathBuf,
}
impl Tasks {
    /// Creates a new Task.
    pub fn create(args: &TaskArgs) -> io::Result<Tasks> {
        let cur_dir = current_dir()?;
        Ok(Tasks {
            queue: Tasks::get_tasks(&cur_dir)?,
            device_num: args.device,
            output_dir: Tasks::create_output_dir(&cur_dir, args.output_dir.as_str()),
        })
    }
    /// Returns a work-stealing queue from which worker threads are going to steal.
    pub fn get_main_worker(self) -> Worker<Option<DirEntry>> {
        self.queue
    }
    /// Returns the specified amount of worker threads to be used.
    pub fn get_device(&self) -> u8 {
        self.device_num - 1
    }
    /// Returns the output directory
    pub fn get_output_dir(&self) -> PathBuf {
        self.output_dir.clone()
    }
    /// Attempts to calculate the upper limit of the amount of work each thread should take.
    pub fn get_task_amount(&self) -> usize {
        {
            if self.device_num > 1 {
                let as_f64 = self.queue.len() as f64 / f64::from(self.device_num).ceil() + 1.0;
                as_f64 as usize
            } else {
                eprintln!("Minimum amount of device: 2");
                std::process::exit(1)
            }
        }
    }
    fn get_tasks(cur_dir: &PathBuf) -> io::Result<Worker<Option<DirEntry>>> {
        let read_dir = std::fs::read_dir(cur_dir)?;
        let worker = Worker::new_fifo();
        let _tasks = read_dir
            .map(|direntry| worker.push(direntry.ok()))
            .collect::<Vec<_>>();
        Ok(worker)
    }
    fn create_output_dir(cur_dir: &Path, output_dir: &str) -> PathBuf {
        let output_path = PathBuf::from(output_dir);
        if !cur_dir.join(output_path.as_path()).exists() {
            if let Err(e) = std::fs::create_dir(output_dir) {
                eprintln!("Cannot create output dir {output_dir}\n{e}")
            }
        }
        output_path
    }
}
/// Worker threads.
pub struct TaskWorker<'a> {
    device_num: u8,
    quality: i32,
    dir_name: PathBuf,
    stealer: &'a Stealer<Option<DirEntry>>,
    task_amount: usize,
}
impl<'a> TaskWorker<'a> {
    /// Creates a new TaskWorker.
    pub fn new(
        device_num: u8,
        quality: i32,
        dir_name: PathBuf,
        stealer: &'a Stealer<Option<DirEntry>>,
        task_amount: usize,
    ) -> Self {
        Self {
            device_num,
            quality,
            dir_name,
            stealer,
            task_amount,
        }
    }
    /// Distribute work among threads.
    /// This method consumes the TaskWorker and returns a vector containing the handles to each thread.
    pub fn send_to_threads(self) -> Option<Vec<thread::JoinHandle<()>>> {
        let device_num_as_usize = usize::from(self.device_num);
        let mut handles = Vec::with_capacity(device_num_as_usize);
        let mut stealers = Vec::with_capacity(device_num_as_usize);
        let mut workers = Vec::with_capacity(device_num_as_usize);
        for _ in 0..self.device_num {
            let thread_worker = Worker::new_fifo();
            let _thread_stealer = self
                .stealer
                .steal_batch_with_limit(&thread_worker, self.task_amount);
            stealers.push(thread_worker.stealer());
            workers.push(thread_worker);
        }
        let to_steal_from = Arc::new(Mutex::new(stealers));
        for _ in 0..self.device_num {
            let thread_worker = workers.pop()?;
            let local_stealer = Arc::clone(&to_steal_from);
            let thread_dir_name = self.dir_name.clone();
            let handle = thread::spawn(move || {
                let mut queues_empty = Vec::with_capacity(device_num_as_usize);
                loop {
                    if let Some(direntry) = thread_worker.pop() {
                        Compress::new(direntry, thread_dir_name.clone(), self.quality).do_work();
                        continue;
                    }
                    let gain_lock = local_stealer.try_lock().ok();
                    let Some(list_of_stealers) = gain_lock else {
                        continue;
                    };
                    for stealer in list_of_stealers.iter() {
                        let Steal::Success(direntry) = stealer.steal() else {
                            continue;
                        };
                        Compress::new(direntry, thread_dir_name.clone(), self.quality).do_work();
                        if stealer.is_empty() {
                            queues_empty.push(true);
                        } else {
                            queues_empty.push(false);
                        }
                    }
                    // If all worker threads have exhausted their queue,
                    // exit this loop
                    if queues_empty.iter().all(|val| val == &true) {
                        break;
                    }
                    queues_empty.clear();
                }
            });
            handles.push(handle);
        }
        Some(handles)
    }
}
/// Compression-related work.
pub struct Compress {
    direntry: Option<DirEntry>,
    dir_name: PathBuf,
    quality: i32,
}
impl Compress {
    /// Creates a new compression task.
    pub fn new(direntry: Option<DirEntry>, dir_name: PathBuf, quality: i32) -> Self {
        Self {
            direntry,
            dir_name,
            quality,
        }
    }
    /// Compresses the image with [turbojpeg](https://github.com/honzasp/rust-turbojpeg).
    pub fn do_work(self) {
        let Some(val_direntry) = self.direntry else {
            return;
        };
        match Compress::compress(val_direntry.path(), self.dir_name, self.quality) {
            Err(e) => {
                eprintln!("{e}");
            }
            Ok(msg) => {
                println!("{msg}");
            }
        };
    }
    fn compress<T>(p: T, dir: PathBuf, q: i32) -> anyhow::Result<String>
    where
        T: AsRef<Path>,
    {
        let path_as_ref = p.as_ref();
        let filename = path_as_ref
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let with_exif_preserved = CompressImage::new(path_as_ref, q)
            .read()?
            .compress()?
            .preserve_exif()?;
        let before_size = with_exif_preserved.format_size_before();
        let after_size = with_exif_preserved.format_size_after();
        std::fs::write(
            dir.join(&filename),
            with_exif_preserved.result().encoder().bytes(),
        )?;
        let success_msg = format!("{filename} before: {before_size} after: {after_size}");
        Ok(success_msg)
    }
}
/// Compress an image, retaining its bytes before and after compression.
struct CompressImage<'a> {
    p: &'a Path,
    q: i32,
    original_bytes: Vec<u8>,
    compressed_bytes: Vec<u8>,
}
impl<'a> CompressImage<'a> {
    /// Creates a new image to be compressed.
    fn new(p: &'a Path, q: i32) -> Self {
        Self {
            p,
            q,
            original_bytes: Vec::new(),
            compressed_bytes: Vec::new(),
        }
    }
    /// Reads image file into Vec<u8>, returning Self.
    fn read(mut self) -> io::Result<Self> {
        self.original_bytes = std::fs::read(self.p)?;
        Ok(self)
    }
    /// Compress image file and retains the compressed bytes, returning Self.
    fn compress(mut self) -> anyhow::Result<Self> {
        let image: image::RgbImage = decompress_image(self.original_bytes.as_bytes())?;
        let jpeg_data = compress_image(&image, self.q, Sub2x2)?;
        self.compressed_bytes = jpeg_data.as_bytes().to_owned();
        Ok(self)
    }
    /// Using the bytes retained before and after compression,
    /// Parse EXIF information from the original bytes and write it
    /// into the compressed bytes. Returns a `img_parts::jpeg::Jpeg`
    /// which we can convert later to a Byte.
    fn preserve_exif(self) -> anyhow::Result<CompressionResult> {
        let before_size = self.original_bytes.len();
        let after_size = self.compressed_bytes.len();
        let original_img_parts = Jpeg::from_bytes(self.original_bytes.into())?;
        let exif = original_img_parts.exif().unwrap_or_default();
        let icc_profile = original_img_parts.icc_profile().unwrap_or_default();
        let mut compressed_img_part = Jpeg::from_bytes(self.compressed_bytes.into())?;
        compressed_img_part.set_exif(exif.into());
        compressed_img_part.set_icc_profile(icc_profile.into());
        Ok(CompressionResult::store(
            compressed_img_part,
            before_size,
            after_size,
        ))
    }
}
/// Contains the result of compressed image
struct CompressionResult {
    compressed_img: img_parts::jpeg::Jpeg,
    before_length: usize,
    after_length: usize,
}
impl CompressionResult {
    /// Store the result of compressed image,
    /// along with additional information.
    fn store(
        compressed_img: img_parts::jpeg::Jpeg,
        before_length: usize,
        after_length: usize,
    ) -> Self {
        Self {
            compressed_img,
            before_length,
            after_length,
        }
    }
    /// Returns the compressed image as a `img_parts_jpeg::Jpeg`.
    fn result(self) -> img_parts::jpeg::Jpeg {
        self.compressed_img
    }
    /// Pretty formatting for original image size.
    fn format_size_before(&self) -> colored::ColoredString {
        let in_mbytes = (self.before_length) as f64 / 1_000_000.0;
        let as_string = format!("{:.2} MB", in_mbytes);
        as_string.bright_red()
    }
    /// Pretty formatting for compressed image size.
    fn format_size_after(&self) -> colored::ColoredString {
        let in_mbytes = (self.after_length) as f64 / 1_000_000.0;
        let as_string = format!("{:.2} MB", in_mbytes);
        as_string.green()
    }
}
