//! Module implementing the thread pool that does the image captioning.
//! This is used by the /caption request handler.

use std::time::Duration;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use antidote::Mutex;
use atomic::Atomic;
use futures::{BoxFuture, future, Future};
use futures_cpupool::{self, CpuPool};
use log::LogLevel::*;
use rand::{self, thread_rng};
use rofl::{self, CaptionOutput, CaptionError, Font, ImageMacro, Template};
use rofl::cache::ThreadSafeCache;
use thread_id;
use tokio_timer::{TimeoutError, Timer, TimerError};

use args::Resource;
use super::{FONT_DIR, TEMPLATE_DIR};
use super::list::{list_templates, list_fonts};


lazy_static! {
    /// The singleton instance of Captioner.
    pub static ref CAPTIONER: Arc<Captioner> = Arc::new(Captioner::new());
}

/// Renders image macros into captioned images.
pub struct Captioner {
    pool: Mutex<CpuPool>,
    engine: rofl::Engine,
    timer: Timer,
    // Configuration params.
    task_timeout: Atomic<Duration>,
}

impl Captioner {
    #[inline]
    fn new() -> Self {
        let pool = Mutex::new(Self::pool_builder().create());
        let engine = Self::engine_builder().build()
            .expect("failed to create rofl::Engine in Captioner::new");
        let timer = Timer::default();

        let task_timeout = Atomic::new(Duration::from_secs(0));

        Captioner{pool, engine, timer, task_timeout}
    }

    #[doc(hidden)]
    fn pool_builder() -> futures_cpupool::Builder {
        let mut builder = futures_cpupool::Builder::new();
        builder.name_prefix("caption-");
        if log_enabled!(Trace) {
            builder.after_start(|| trace!(
                "Worker thread (ID={:#x}) created in Captioner::pool",
                thread_id::get()));
            builder.before_stop(|| trace!(
                "Stopping worker thread (ID={:#x}) in Captioner::pool",
                thread_id::get()));
        }
        builder
    }

    #[doc(hidden)]
    fn engine_builder() -> rofl::EngineBuilder {
        rofl::EngineBuilder::new()
            .template_directory(&*TEMPLATE_DIR)
            .font_directory(&*FONT_DIR)
    }
}

impl Captioner {
    #[inline]
    pub fn template_cache(&self) -> &ThreadSafeCache<String, Template> {
        self.engine.template_cache().unwrap()
    }

    #[inline]
    pub fn font_cache(&self) -> &ThreadSafeCache<String, Font> {
        self.engine.font_cache().unwrap()
    }
}

// Configuration tweaks.
impl Captioner {
    #[inline]
    pub fn set_thread_count(&self, count: usize) -> &Self {
        trace!("Setting thread count for image captioning to {}", count);

        let mut builder = Self::pool_builder();
        if count > 0 {
            builder.pool_size(count);
        }

        let pool = builder.create();
        *self.pool.lock() = pool;
        self
    }

    #[inline]
    pub fn set_task_timeout(&self, timeout: Duration) -> &Self {
        let secs = timeout.as_secs();
        if secs > 0 {
            trace!("Setting caption request timeout to {} secs", secs);
        } else {
            trace!("Disabling caption request timeout");
        }
        self.task_timeout.store(timeout, Ordering::Relaxed);
        self
    }

    /// Fill the cache for given type of resource.
    pub fn preload(&self, what: Resource) {
        let mut rng = thread_rng();
        match what {
            Resource::Template => {
                let capacity = self.template_cache().capacity();
                debug!("Preloading up to {} templates", capacity);
                // TODO: the sampling here is O(N_t*C), so it can be quadratic;
                // pick a better method (probably the random_choice crate)
                for template in rand::sample(&mut rng, list_templates(), capacity) {
                    if let Err(e) = self.engine.preload_template(&template) {
                        warn!("Error preloading template `{}`: {}", template, e);
                    }
                }
            }
            Resource::Font => {
                let capacity = self.font_cache().capacity();
                debug!("Preloading up to {} fonts", capacity);
                for font in rand::sample(&mut rng, list_fonts(), capacity) {
                    if let Err(e) = self.engine.preload_font(&font) {
                        warn!("Error preloading font `{}`: {}", font, e);
                    }
                }
            }
        }
    }

    #[inline]
    pub fn set_jpeg_quality(&self, quality: u8) -> bool {
        trace!("Setting generated JPEG quality to {}%", quality);
        if !(0 < quality && quality <= 100) {
            warn!("JPEG quality out of range: {}%", quality);
            return false;
        }
        self.engine.config_mut().jpeg_quality = quality;
        true
    }

    #[inline]
    pub fn set_gif_quality(&self, quality: u8) -> bool {
        trace!("Setting quality of generated GIF animations to {}%", quality);
        if !(0 < quality && quality <= 100) {
            warn!("GIF animation quality out of range: {}%", quality);
            return false;
        }
        self.engine.config_mut().gif_quality = quality;
        true
    }
}

// Rendering code.
impl Captioner {
    /// Render an image macro as PNG.
    /// The rendering is done in a separate thread.
    pub fn render(&self, im: ImageMacro) -> BoxFuture<CaptionOutput, RenderError> {
        let pool = match self.pool.try_lock() {
            Ok(p) => p,
            Err(_) => {
                // Indicates we'd have to wait for the pool lock.
                // This should be only possible when set_thread_count() happens
                // to have been called at the exact same moment.
                warn!("Could not immediately lock CpuPool to render {:?}", im);
                // TODO: retry a few times, probably with exponential backoff
                return future::err(RenderError::Unavailable).boxed();
            },
        };

        // Spawn a new task in the thread pool for the rendering process.
        let task_future = pool.spawn_fn({
            let im_repr = format!("{:?}", im);
            let engine = self.engine.clone();
            move || {
                match engine.caption(im) {
                    Ok(out) => {
                        debug!("Successfully rendered {} as {:?}, final result size: {} bytes",
                            im_repr, out.format(), out.len());
                        future::ok(out)
                    },
                    Err(e) => {
                        error!("Failed to render image macro {}: {}", im_repr, e);
                        future::err(e)
                    },
                }
            }
        }).map_err(RenderError::Caption);

        // Impose a timeout on the task.
        let max_duration = self.task_timeout.load(Ordering::Relaxed);
        if max_duration.as_secs() > 0 {
            // TODO: this doesn't seem to actually kill the underlying thread,
            // figure out how to do that
            self.timer.timeout(task_future, max_duration).boxed()
        } else {
            task_future.boxed()
        }
    }
}


/// Error that can occur during the image macro rendering process.
#[derive(Debug, Error)]
pub enum RenderError {
    /// Error during the captioning process.
    Caption(CaptionError),
    /// Timeout while performing the caption request.
    Timeout,
    /// Captioning service temporarily unavailable.
    Unavailable,
}

// Necessary for imposing a timeout on the CaptionTask.
impl<F> From<TimeoutError<F>> for RenderError {
    fn from(e: TimeoutError<F>) -> Self {
        match e {
            TimeoutError::Timer(_, TimerError::NoCapacity) => RenderError::Unavailable,
            _ => RenderError::Timeout,
        }
    }
}
