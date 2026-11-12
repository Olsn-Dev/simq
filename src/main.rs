// No console window on Windows in release builds.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use anyhow::{anyhow, Context};
use eframe::egui;

use simq::config;
use simq::i18n::{self, t};
use simq::store::Store;
use simq::sync::Client;
use simq::ui::App;

const USAGE: &str = "SimQ – kö för simuleringsförfrågor / queue for simulation requests

Användning / Usage: simq [--config-dir <sökväg/path>]

  --config-dir <sökväg/path>  Lägg inställningar och cache i den här mappen, så att
                              flera instanser kan köras på samma dator.
                              Keep settings and cache in this folder, so that several
                              instances can run on the same computer.
  --version                   Visa versionen / Show the version.
  --help                      Visa den här hjälpen / Show this help.";

fn main() -> anyhow::Result<()> {
    let args =
        config::parse_args(std::env::args().skip(1)).map_err(|e| anyhow!("{e}\n\n{USAGE}"))?;
    if args.help {
        println!("{USAGE}");
        return Ok(());
    }
    if args.version {
        println!("simq {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    let paths = config::resolve_paths(args.config_dir.as_deref())?;
    let cfg = config::load_or_init(&paths)
        .context("kunde inte läsa inställningarna / could not read the settings")?;
    i18n::set(cfg.language);
    let store = Store::open_or_recreate(&paths.cache_file).context(t(
        "kunde inte öppna den lokala cachen",
        "could not open the local cache",
    ))?;
    let client = Client::new(
        store,
        cfg.client_id,
        cfg.display_name.clone(),
        cfg.effective_folder(&paths),
    )
    .context(t(
        "kunde inte läsa den lokala cachen",
        "could not read the local cache",
    ))?;

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("SimQ")
            .with_inner_size([1180.0, 720.0])
            .with_min_inner_size([820.0, 480.0])
            .with_icon(
                eframe::icon_data::from_png_bytes(simq::theme::ICON_PNG).unwrap_or_default(),
            ),
        ..Default::default()
    };
    eframe::run_native(
        "simq",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, client, cfg, paths)?))),
    )
    .map_err(|e| {
        anyhow!(
            "{}: {e}",
            t(
                "kunde inte starta gränssnittet",
                "could not start the user interface"
            )
        )
    })
}
