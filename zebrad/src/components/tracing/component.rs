//! The Abscissa component for Zebra's `tracing` implementation.

use std::{
    fs::{self, File},
    io::Write,
};

use abscissa_core::{Component, FrameworkError, Shutdown};
use tracing_appender::non_blocking::{NonBlocking, NonBlockingBuilder, WorkerGuard};
use tracing_error::ErrorLayer;
use tracing_subscriber::{
    fmt::{format, Formatter},
    layer::SubscriberExt,
    reload::Handle,
    util::SubscriberInitExt,
    EnvFilter,
};

use crate::{application::app_version, components::tracing::Config};

#[cfg(feature = "flamegraph")]
use super::flame;

/// A type-erased boxed writer that can be sent between threads safely.
pub type BoxWrite = Box<dyn Write + Send + Sync + 'static>;

/// Abscissa component for initializing the `tracing` subsystem
pub struct Tracing {
    /// The installed filter reloading handle, if enabled.
    //
    // TODO: when fmt::Subscriber supports per-layer filtering, remove the Option
    filter_handle: Option<
        Handle<
            EnvFilter,
            Formatter<format::DefaultFields, format::Format<format::Full>, NonBlocking>,
        >,
    >,

    /// The originally configured filter.
    initial_filter: String,

    /// The installed flame graph collector, if enabled.
    #[cfg(feature = "flamegraph")]
    flamegrapher: Option<flame::Grapher>,

    /// Drop guard for worker thread of non-blocking logger,
    /// responsible for flushing any remaining logs when the program terminates.
    //
    // Correctness: must be listed last in the struct, so it drops after other drops have logged.
    _guard: WorkerGuard,
}

impl Tracing {
    /// Try to create a new [`Tracing`] component with the given `filter`.
    #[allow(clippy::print_stdout, clippy::print_stderr)]
    pub fn new(config: Config) -> Result<Self, FrameworkError> {
        let filter = config.filter.unwrap_or_default();
        let flame_root = &config.flamegraph;

        let writer = if let Some(log_file) = config.log_file.as_ref() {
            println!("running zebra");

            // Make sure the directory for the log file exists.
            // If the log is configured in the current directory, it won't have a parent directory.
            //
            // # Security
            //
            // If the user is running Zebra with elevated permissions ("root"), they should
            // create the log file directory before running Zebra, and make sure the Zebra user
            // account has exclusive access to that directory, and other users can't modify
            // its parent directories.
            //
            // This avoids a TOCTOU security issue in the Rust filesystem API.
            let log_file_dir = log_file.parent();
            if let Some(log_file_dir) = log_file_dir {
                if !log_file_dir.exists() {
                    println!("directory for log file {log_file:?} does not exist, trying to create it...");

                    if let Err(create_dir_error) = fs::create_dir_all(log_file_dir) {
                        println!("failed to create directory for log file: {create_dir_error}");
                        println!("trying log file anyway...");
                    }
                }
            }

            println!("sending logs to {log_file:?}...");
            let log_file = File::options().append(true).create(true).open(log_file)?;
            Box::new(log_file) as BoxWrite
        } else {
            let stdout = std::io::stdout();
            Box::new(stdout) as BoxWrite
        };

        // Builds a lossy NonBlocking logger with a default line limit of 128_000 or an explicit buffer_limit.
        // The write method queues lines down a bounded channel with this capacity to a worker thread that writes to stdout.
        // Increments error_counter and drops lines when the buffer is full.
        let (non_blocking, _guard) = NonBlockingBuilder::default()
            .buffered_lines_limit(config.buffer_limit.max(100))
            .finish(writer);

        // Only use color if tracing output is being sent to a terminal or if it was explicitly
        // forced to.
        let use_color =
            config.force_use_color || (config.use_color && atty::is(atty::Stream::Stdout));

        // Construct a format subscriber with the supplied global logging filter,
        // and optionally enable reloading.
        //
        // TODO: when fmt::Subscriber supports per-layer filtering, always enable this code
        #[cfg(not(all(feature = "tokio-console", tokio_unstable)))]
        let (subscriber, filter_handle) = {
            use tracing_subscriber::FmtSubscriber;

            let logger = FmtSubscriber::builder()
                .with_ansi(use_color)
                .with_writer(non_blocking)
                .with_env_filter(&filter);

            // Enable reloading if that feature is selected.
            #[cfg(feature = "filter-reload")]
            let (filter_handle, logger) = {
                let logger = logger.with_filter_reloading();

                (Some(logger.reload_handle()), logger)
            };

            #[cfg(not(feature = "filter-reload"))]
            let filter_handle = None;

            let subscriber = logger.finish().with(ErrorLayer::default());

            (subscriber, filter_handle)
        };

        // Construct a tracing registry with the supplied per-layer logging filter,
        // and disable filter reloading.
        //
        // TODO: when fmt::Subscriber supports per-layer filtering,
        //       remove this registry code, and layer tokio-console on top of fmt::Subscriber
        #[cfg(all(feature = "tokio-console", tokio_unstable))]
        let (subscriber, filter_handle) = {
            use tracing_subscriber::{fmt, Layer};

            let subscriber = tracing_subscriber::registry();
            // TODO: find out why crawl_and_dial and try_to_sync evade this filter,
            //       and why they also don't get the global net/commit span.
            // Note: this might have been fixed by tracing 0.3.15, or by recent Zebra refactors.
            //
            // Using `registry` as the base subscriber, the logs from most other functions get filtered.
            // Using `FmtSubscriber` as the base subscriber, all the logs get filtered.
            let logger = fmt::Layer::new()
                .with_ansi(use_color)
                .with_writer(non_blocking)
                .with_filter(EnvFilter::from(&filter));

            let subscriber = subscriber.with(logger);

            let span_logger = ErrorLayer::default().with_filter(EnvFilter::from(&filter));
            let subscriber = subscriber.with(span_logger);

            (subscriber, None)
        };

        // Add optional layers based on dynamic and compile-time configs

        // Add a flamegraph
        #[cfg(feature = "flamegraph")]
        let (flamelayer, flamegrapher) = if let Some(path) = flame_root {
            let (flamelayer, flamegrapher) = flame::layer(path);

            (Some(flamelayer), Some(flamegrapher))
        } else {
            (None, None)
        };
        #[cfg(feature = "flamegraph")]
        let subscriber = subscriber.with(flamelayer);

        #[cfg(feature = "journald")]
        let journaldlayer = if config.use_journald {
            use abscissa_core::FrameworkErrorKind;

            let layer = tracing_journald::layer()
                .map_err(|e| FrameworkErrorKind::ComponentError.context(e))?;

            // If the global filter can't be used, add a per-layer filter instead.
            // TODO: when fmt::Subscriber supports per-layer filtering, always enable this code
            #[cfg(all(feature = "tokio-console", tokio_unstable))]
            let layer = {
                use tracing_subscriber::Layer;
                layer.with_filter(EnvFilter::from(&filter))
            };

            Some(layer)
        } else {
            None
        };
        #[cfg(feature = "journald")]
        let subscriber = subscriber.with(journaldlayer);

        #[cfg(feature = "sentry")]
        let subscriber = subscriber.with(sentry::integrations::tracing::layer());

        // spawn the console server in the background, and apply the console layer
        // TODO: set Builder::poll_duration_histogram_max() if needed
        #[cfg(all(feature = "tokio-console", tokio_unstable))]
        let subscriber = subscriber.with(console_subscriber::spawn());

        // Initialise the global tracing subscriber
        subscriber.init();

        // Log the tracing stack we just created
        tracing::info!(
            ?filter,
            TRACING_STATIC_MAX_LEVEL = ?tracing::level_filters::STATIC_MAX_LEVEL,
            LOG_STATIC_MAX_LEVEL = ?log::STATIC_MAX_LEVEL,
            "started tracing component",
        );

        if flame_root.is_some() {
            if cfg!(feature = "flamegraph") {
                info!(flamegraph = ?flame_root, "installed flamegraph tracing layer");
            } else {
                warn!(
                    flamegraph = ?flame_root,
                    "unable to activate configured flamegraph: \
                     enable the 'flamegraph' feature when compiling zebrad",
                );
            }
        }

        if config.use_journald {
            if cfg!(feature = "journald") {
                info!("installed journald tracing layer");
            } else {
                warn!(
                    "unable to activate configured journald tracing: \
                     enable the 'journald' feature when compiling zebrad",
                );
            }
        }

        #[cfg(feature = "sentry")]
        info!("installed sentry tracing layer");

        #[cfg(all(feature = "tokio-console", tokio_unstable))]
        info!(
            TRACING_STATIC_MAX_LEVEL = ?tracing::level_filters::STATIC_MAX_LEVEL,
            LOG_STATIC_MAX_LEVEL = ?log::STATIC_MAX_LEVEL,
            "installed tokio-console tracing layer",
        );

        // Write any progress reports sent by other tasks to the terminal
        //
        // TODO: move this to its own module?
        #[cfg(feature = "progress-bar")]
        {
            use howudoin::consumers::TermLine;
            use std::time::Duration;

            // Stops flickering during the initial sync.
            const PROGRESS_BAR_DEBOUNCE: Duration = Duration::from_secs(2);

            let terminal_consumer = TermLine::with_debounce(PROGRESS_BAR_DEBOUNCE);
            howudoin::init(terminal_consumer);

            info!("activated progress bar");

            if config.log_file.is_some() {
                eprintln!("waiting for initial progress reports...");
            }
        }

        Ok(Self {
            filter_handle,
            initial_filter: filter,
            #[cfg(feature = "flamegraph")]
            flamegrapher,
            _guard,
        })
    }

    /// Return the currently-active tracing filter.
    pub fn filter(&self) -> String {
        if let Some(filter_handle) = self.filter_handle.as_ref() {
            filter_handle
                .with_current(|filter| filter.to_string())
                .expect("the subscriber is not dropped before the component is")
        } else {
            self.initial_filter.clone()
        }
    }

    /// Reload the currently-active filter with the supplied value.
    ///
    /// This can be used to provide a dynamic tracing filter endpoint.
    pub fn reload_filter(&self, filter: impl Into<EnvFilter>) {
        let filter = filter.into();

        if let Some(filter_handle) = self.filter_handle.as_ref() {
            tracing::info!(
                ?filter,
                TRACING_STATIC_MAX_LEVEL = ?tracing::level_filters::STATIC_MAX_LEVEL,
                LOG_STATIC_MAX_LEVEL = ?log::STATIC_MAX_LEVEL,
                "reloading tracing filter",
            );

            filter_handle
                .reload(filter)
                .expect("the subscriber is not dropped before the component is");
        } else {
            tracing::warn!(
                ?filter,
                TRACING_STATIC_MAX_LEVEL = ?tracing::level_filters::STATIC_MAX_LEVEL,
                LOG_STATIC_MAX_LEVEL = ?log::STATIC_MAX_LEVEL,
                "attempted to reload tracing filter, but filter reloading is disabled",
            );
        }
    }
}

impl std::fmt::Debug for Tracing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tracing").finish()
    }
}

impl<A: abscissa_core::Application> Component<A> for Tracing {
    fn id(&self) -> abscissa_core::component::Id {
        abscissa_core::component::Id::new("zebrad::components::tracing::component::Tracing")
    }

    fn version(&self) -> abscissa_core::Version {
        app_version()
    }

    fn before_shutdown(&self, _kind: Shutdown) -> Result<(), FrameworkError> {
        #[cfg(feature = "flamegraph")]
        if let Some(ref grapher) = self.flamegrapher {
            use abscissa_core::FrameworkErrorKind;

            info!("writing flamegraph");

            grapher
                .write_flamegraph()
                .map_err(|e| FrameworkErrorKind::ComponentError.context(e))?
        }

        #[cfg(feature = "progress-bar")]
        howudoin::disable();

        Ok(())
    }
}

impl Drop for Tracing {
    fn drop(&mut self) {
        #[cfg(feature = "progress-bar")]
        howudoin::disable();
    }
}
